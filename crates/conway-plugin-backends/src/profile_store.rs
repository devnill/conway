//! **The** profile facility: kind-agnostic storage, selection, merge order
//! and error reporting for a named bundle of configuration a backend entry
//! may select by id. Everything in this module is generic over the payload
//! (`T: Profiled`) a kind chooses to resolve a name to -- this module itself
//! never reads a field beyond `id`, so it cannot become dialect-specific by
//! accident.
//!
//! # Why this is ONE type, not a store per kind (S4b)
//!
//! `openai_compat`'s `Profile` (`crate::profile`) is a nine-field
//! wire-behavior/capability bundle: `chat_path`, `supports_stream_options`,
//! `tool_call_style`, and five more fields that are genuinely
//! OpenAI-compatible-wire-shaped, plus six fields
//! (`crate::capabilities::DialectDefaults`'s own shape) that are not
//! wire-specific at all -- they are baseline *capability* data any kind's
//! model could plausibly declare. `"anthropic"` has no analogue of the first
//! group (the Messages API has exactly one wire shape --
//! `crate::factory`'s own module doc records that finding, from the item this
//! one depends on) and, as of this item, no shipped built-in profiles of the
//! second group either (a later item populates that; see `factory.rs`'s
//! `AnthropicBackendFactory::resolve_fields` module doc for what it validates
//! today: `anthropic_version`/`headers`).
//!
//! Two per-kind stores each hard-coding their own answers to "what may a
//! profile override," "how is an unknown key handled," and "what precedence
//! does a profile have against explicit configuration" is exactly the drift
//! this item exists to prevent (its own spec's framing). So this module owns
//! all three of those questions ONCE:
//!
//! - **What a profile may override:** nothing, as far as this module is
//!   concerned -- `Profiled::parse_source` is the kind's own conversion from
//!   a `[[profile]]` TOML entry to whatever typed (`crate::profile::Profile`)
//!   or untyped ([`ProfileBundle`]) shape it wants; this module stores and
//!   indexes the RESULT, never inspects it.
//! - **How an unknown profile NAME is handled:** [`ProfileStore::resolve`]
//!   -- a typed, named [`crate::config::ConfigError::UnknownProfile`], never
//!   a silent default. (An unknown FIELD inside a profile entry is the
//!   kind's own concern -- `crate::profile::ProfileRaw`'s
//!   `deny_unknown_fields` is `openai_compat`'s answer;
//!   `AnthropicBackendFactory::resolve_fields`'s exhaustive `match` is
//!   `"anthropic"`'s.)
//! - **Profile vs. explicit configuration vs. defaults:** [`apply_precedence`]
//!   -- one function, one documented rule, called by every kind that reads
//!   both a profile and an explicit `extra` map.
//!
//! A test over the TYPES (not a reviewer's reading) pins this:
//! `tests/profile_facility.rs`'s `exactly_one_profile_store_type_exists`
//! greps this crate's own `src/` for every `struct` whose name contains
//! `ProfileStore` and asserts there is exactly one definition -- this one,
//! generic over `T`. A stub second store (even a zero-field one) fails it
//! immediately; the break-the-guard run recorded in that test's own doc
//! proves this.
//!
//! # A finding, recorded rather than engineered around: one facility does
//! # NOT mean one physical profile FILE is shareable across kinds
//!
//! `ProfileStore::merge_file`/`from_source` delegate parsing the ENTIRE
//! source string to `T::parse_source` -- there is no way for this module to
//! skip an entry meant for a different kind's vocabulary, since it never
//! looks inside an entry at all. `Profile::parse_source`
//! (`crate::profile`) deserializes through `ProfileFile { profile:
//! Vec<Profile> }` with `#[serde(deny_unknown_fields)]` on every entry, so a
//! `[[profile]]` file containing even one `"anthropic"`-shaped entry
//! (`anthropic_version`, `headers`) fails to load AT ALL for
//! `"openai-compat"` -- loudly, which is correct (the alternative,
//! tolerating unrecognized fields crate-wide, reopens the exact silent-typo
//! defect `deny_unknown_fields` exists to close). `tests/
//! profile_facility.rs`'s verification-anchor tests discovered this by
//! trying one shared file first; its own module doc records the production
//! consequence: an operator who wants both kinds' profiles discoverable
//! needs them in DIFFERENT files (project- vs. global-scoped, say), not one
//! mixed one. This is the facility staying genuinely kind-agnostic at the
//! COST of one physical file not being universally shareable -- the
//! alternative (a facility that peeks inside entries to route them) would
//! be the facility becoming dialect-aware, which is the outcome this item
//! exists to prevent.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::ConfigError;

/// Where one loaded entry came from -- the "what is loaded" inspection
/// surface every `ProfileStore<T>::list()` exposes, mirroring the same
/// principle `conway_runtime::permission`'s `active_patterns()` establishes
/// for permission rules: a rule set (here, a resolved-configuration set)
/// nobody can inspect is a trap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileOrigin {
    /// Parsed from a kind's own compile-time-embedded source
    /// (`ProfileStore::from_source`).
    BuiltIn,
    /// Loaded from a file at this path (project- or global-scoped
    /// `.conway/profiles.toml`; see `conway::config::discovery`).
    File(PathBuf),
}

/// One entry in a [`ProfileStore`]: the kind-resolved payload plus where it
/// came from, and -- when it replaced an already-loaded entry with the same
/// id -- where *that* one came from. Overriding a profile (a project file
/// shadowing a built-in, or a global file shadowing a project one) is a
/// deliberate, supported mechanism; `shadows` is what keeps that override
/// visible rather than silent.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedProfile<T> {
    pub profile: T,
    pub origin: ProfileOrigin,
    pub shadows: Option<ProfileOrigin>,
}

/// What a kind's own profile payload must supply so the facility can index,
/// merge and error on it without knowing what any OTHER field means: a
/// selection key ([`Profiled::id`]) and a way to parse a `[[profile]]`
/// array-of-tables source string into a `Vec<Self>` ([`Profiled::
/// parse_source`]) -- the kind's own typed (`crate::profile::Profile`) or
/// untyped ([`ProfileBundle`]) deserialization, deliberately opaque to this
/// module.
pub trait Profiled: Sized {
    /// The name a backend config selects this profile by. Never empty --
    /// `parse_source` implementations reject an empty id themselves (see
    /// `crate::profile::Profile`'s `TryFrom<ProfileRaw>`/[`ProfileBundle::
    /// parse_source`] for each kind's own check); this module does not
    /// re-validate it, only indexes by whatever is returned.
    fn id(&self) -> &str;

    /// Parses `source` (a `[[profile]]` TOML array-of-tables document,
    /// identical shape for every kind) into every entry it declares. `Err`
    /// names what went wrong (a syntax error, an unrecognized field, an
    /// empty id) -- the kind's own vocabulary, wrapped by [`ProfileStore::
    /// merge_file`]/`from_source` into a [`ConfigError`] that also names the
    /// source (a path, for a loaded file).
    fn parse_source(source: &str) -> Result<Vec<Self>, String>;
}

/// **The** profile facility (S4b): a resolved set of `T`s, generic over
/// which kind's own payload type `T` is, each entry tracking its
/// [`ProfileOrigin`]. See this module's own doc for why one generic type
/// serves every kind rather than one store per kind.
#[derive(Debug, Clone)]
pub struct ProfileStore<T> {
    entries: BTreeMap<String, LoadedProfile<T>>,
}

impl<T> Default for ProfileStore<T> {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }
}

impl<T: Profiled> ProfileStore<T> {
    /// An empty store -- the starting point for a kind that ships no
    /// compile-time built-ins of its own (today, `"anthropic"`; see
    /// `crate::factory`'s module doc for why it has none YET). Distinct from
    /// [`Self::from_source`] with an empty string on purpose: an empty
    /// source is still a `[[profile]]` document (zero entries), while this
    /// constructor makes "no built-ins at all" the caller's explicit choice
    /// rather than an incidentally-empty parse.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Parses `source` (a `[[profile]]` TOML document) into a store whose
    /// every entry's origin is [`ProfileOrigin::BuiltIn`] -- a kind's own
    /// compile-time-embedded profile set, the starting point
    /// [`Self::merge_file`] layers project/global files over.
    pub fn from_source(source: &str) -> Result<Self, String> {
        let mut entries = BTreeMap::new();
        for profile in T::parse_source(source)? {
            entries.insert(
                profile.id().to_string(),
                LoadedProfile {
                    profile,
                    origin: ProfileOrigin::BuiltIn,
                    shadows: None,
                },
            );
        }
        Ok(Self { entries })
    }

    /// Reads `path` as a `[[profile]]` array-of-tables TOML file and layers
    /// its entries over `self`, id-for-id; a same-id entry replaces the
    /// existing one and records its previous origin in [`LoadedProfile::
    /// shadows`] (visible shadowing -- never silent).
    ///
    /// A nonexistent path returns `self` unchanged, not an error -- every
    /// discovered profile file path is optional (mirrors `ModelMetadataStore
    /// ::load`'s identical "missing is not an error" contract). Any other
    /// I/O failure, or a syntactically/structurally invalid file (whatever
    /// `T::parse_source` rejects -- an unrecognized field, an empty id, ...)
    /// is `Err(ConfigError::Profile { .. })` naming `path` and the detail.
    pub fn merge_file(mut self, path: &Path) -> Result<Self, ConfigError> {
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(self),
            Err(err) => {
                return Err(ConfigError::Profile {
                    path: path.display().to_string(),
                    detail: err.to_string(),
                });
            }
        };
        let profiles = T::parse_source(&content).map_err(|detail| ConfigError::Profile {
            path: path.display().to_string(),
            detail,
        })?;
        for profile in profiles {
            let id = profile.id().to_string();
            let shadows = self.entries.get(&id).map(|prev| prev.origin.clone());
            self.entries.insert(
                id,
                LoadedProfile {
                    profile,
                    origin: ProfileOrigin::File(path.to_path_buf()),
                    shadows,
                },
            );
        }
        Ok(self)
    }

    /// Looks up a profile by id (built-in or loaded). `None`, never an
    /// error -- use [`Self::resolve`] when an unknown name should be a typed
    /// rejection rather than an `Option` the caller must itself convert.
    pub fn get(&self, id: &str) -> Option<&T> {
        self.entries.get(id).map(|loaded| &loaded.profile)
    }

    /// Looks up a profile by id, or a typed [`ConfigError::UnknownProfile`]
    /// naming it -- the facility's OWN answer to "how is an unknown profile
    /// name handled," shared by every kind rather than each hand-rolling its
    /// own wording (this module's own doc, "What a profile may
    /// override"... section). A kind that wraps this with backend-instance
    /// context (e.g. `"backend '{id}': {e}"`) is adding WHERE the failure
    /// happened, never changing WHAT failed.
    pub fn resolve(&self, id: &str) -> Result<&T, ConfigError> {
        self.get(id)
            .ok_or_else(|| ConfigError::UnknownProfile { id: id.to_string() })
    }

    /// Every loaded profile, in id order -- the mandatory "what is loaded"
    /// surface: a caller can always ask which profiles exist and where each
    /// came from (built-in, or which file, and what it shadowed).
    pub fn list(&self) -> Vec<&LoadedProfile<T>> {
        self.entries.values().collect()
    }

    /// Number of loaded profiles.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no profile is loaded (always true for [`Self::empty`] with no
    /// file merged; never true for a store built from a nonempty built-in
    /// source).
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A named, kind-opaque bundle of profile fields: the payload a kind with no
/// pre-existing typed profile shape of its own (today, `"anthropic"` --
/// `crate::factory::AnthropicBackendFactory`) resolves a selected name to.
/// This module never reads a key inside `fields`; a kind interprets it
/// however it validates its own configuration -- see
/// `AnthropicBackendFactory::resolve_fields`, which validates this same shape
/// (`BTreeMap<String, serde_json::Value>`) whether the keys came from a
/// resolved profile, `[backends.<id>].extra`, or (per [`apply_precedence`])
/// both merged.
///
/// `serde_json::Value` rather than `toml::Value` for [`Self::fields`]
/// deliberately: it is the EXACT type `conway_core::ports::
/// BackendBuildContext::extra` already uses, so [`apply_precedence`] merges
/// two maps of one type rather than converting between two.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProfileBundle {
    pub id: String,
    pub fields: BTreeMap<String, serde_json::Value>,
}

impl Profiled for ProfileBundle {
    fn id(&self) -> &str {
        &self.id
    }

    fn parse_source(source: &str) -> Result<Vec<Self>, String> {
        #[derive(serde::Deserialize)]
        struct RawFile {
            #[serde(default)]
            profile: Vec<toml::value::Table>,
        }
        let file: RawFile = toml::from_str(source).map_err(|err| err.to_string())?;
        file.profile
            .into_iter()
            .map(|mut table| {
                let id_value = table
                    .remove("id")
                    .ok_or_else(|| "profile entry is missing 'id'".to_string())?;
                let id = id_value
                    .as_str()
                    .ok_or_else(|| "profile 'id' must be a string".to_string())?
                    .to_string();
                if id.trim().is_empty() {
                    return Err("id must not be empty".to_string());
                }
                let fields = table
                    .into_iter()
                    .map(|(key, value)| {
                        serde_json::to_value(value)
                            .map(|json| (key.clone(), json))
                            .map_err(|err| format!("field '{key}': {err}"))
                    })
                    .collect::<Result<BTreeMap<String, serde_json::Value>, String>>()?;
                Ok(ProfileBundle { id, fields })
            })
            .collect()
    }
}

/// **THE** precedence rule between a resolved profile's fields and a
/// backend's own explicit `extra` map -- one function, stated once, applied
/// identically by every kind that reads both (today: `"anthropic"`'s
/// `AnthropicBackendFactory::build`; `"openai-compat"` selects a profile but
/// reads no `extra` of its own -- see `crate::factory`'s module doc for why
/// the two kinds do not share one `dialect`/`extra` vocabulary, a finding
/// from the item this one depends on).
///
/// **Explicit `extra` wins key-for-key over the profile's value for that
/// key; a key set by neither is left absent from the result, for the
/// caller's own hardcoded default to fill in.** This module does not know
/// what a "default" is for any given key -- only that explicit configuration
/// beats a named preset, which beats nothing. A profile can be reused
/// across many `[backends.<id>]` entries; `extra` is inherently
/// per-instance, so a per-instance override winning over a shared preset is
/// the only order that lets one entry deviate from a profile without
/// forking it.
pub fn apply_precedence(
    profile: Option<&ProfileBundle>,
    extra: &BTreeMap<String, serde_json::Value>,
) -> BTreeMap<String, serde_json::Value> {
    let mut merged = profile
        .map(|bundle| bundle.fields.clone())
        .unwrap_or_default();
    merged.extend(
        extra
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    struct TestProfile {
        id: String,
        note: String,
    }

    impl Profiled for TestProfile {
        fn id(&self) -> &str {
            &self.id
        }

        fn parse_source(source: &str) -> Result<Vec<Self>, String> {
            #[derive(serde::Deserialize)]
            struct Raw {
                id: String,
                #[serde(default)]
                note: String,
            }
            #[derive(serde::Deserialize)]
            struct RawFile {
                #[serde(default)]
                profile: Vec<Raw>,
            }
            let file: RawFile = toml::from_str(source).map_err(|err| err.to_string())?;
            Ok(file
                .profile
                .into_iter()
                .map(|raw| TestProfile {
                    id: raw.id,
                    note: raw.note,
                })
                .collect())
        }
    }

    #[test]
    fn from_source_indexes_by_id_with_built_in_origin() {
        let store = ProfileStore::<TestProfile>::from_source(
            r#"
            [[profile]]
            id = "a"
            note = "first"
            [[profile]]
            id = "b"
            "#,
        )
        .unwrap();
        assert_eq!(store.len(), 2);
        assert_eq!(store.get("a").unwrap().note, "first");
        assert!(store
            .list()
            .iter()
            .all(|l| l.origin == ProfileOrigin::BuiltIn));
    }

    #[test]
    fn resolve_names_the_unknown_id_as_a_typed_error() {
        let store = ProfileStore::<TestProfile>::empty();
        let err = store.resolve("nope").expect_err("must reject unknown id");
        match err {
            ConfigError::UnknownProfile { id } => assert_eq!(id, "nope"),
            other => panic!("expected UnknownProfile, got {other:?}"),
        }
    }

    #[test]
    fn merge_file_missing_path_is_a_no_op_not_an_error() {
        let path = std::env::temp_dir().join("conway-profile-store-does-not-exist.toml");
        let _ = std::fs::remove_file(&path);
        let store = ProfileStore::<TestProfile>::empty();
        let after = store.merge_file(&path).unwrap();
        assert!(after.is_empty());
    }

    #[test]
    fn merge_file_records_a_shadow_when_an_id_already_exists() {
        let dir = std::env::temp_dir().join(format!(
            "conway-profile-store-shadow-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("profiles.toml");
        std::fs::write(
            &path,
            r#"
            [[profile]]
            id = "a"
            note = "overridden"
            "#,
        )
        .unwrap();

        let store = ProfileStore::<TestProfile>::from_source(
            r#"
            [[profile]]
            id = "a"
            note = "original"
            "#,
        )
        .unwrap()
        .merge_file(&path)
        .unwrap();

        let loaded = store
            .list()
            .into_iter()
            .find(|l| l.profile.id == "a")
            .unwrap();
        assert_eq!(loaded.origin, ProfileOrigin::File(path.clone()));
        assert_eq!(loaded.shadows, Some(ProfileOrigin::BuiltIn));
        assert_eq!(loaded.profile.note, "overridden");

        std::fs::remove_dir_all(&dir).ok();
    }

    // --- `ProfileBundle` ---------------------------------------------------

    #[test]
    fn profile_bundle_parses_id_and_leaves_the_rest_as_opaque_fields() {
        let bundles = ProfileBundle::parse_source(
            r#"
            [[profile]]
            id = "gateway"
            anthropic_version = "2024-01-01"

            [profile.headers]
            "x-beta" = "on"
            "#,
        )
        .unwrap();
        assert_eq!(bundles.len(), 1);
        let bundle = &bundles[0];
        assert_eq!(bundle.id, "gateway");
        assert_eq!(
            bundle.fields.get("anthropic_version"),
            Some(&serde_json::json!("2024-01-01"))
        );
        assert_eq!(
            bundle.fields.get("headers"),
            Some(&serde_json::json!({"x-beta": "on"}))
        );
    }

    #[test]
    fn profile_bundle_rejects_a_missing_or_empty_id() {
        assert!(ProfileBundle::parse_source(r#"[[profile]]"#).is_err());
        assert!(ProfileBundle::parse_source(
            r#"[[profile]]
id = ""
"#
        )
        .is_err());
    }

    // --- `apply_precedence` -------------------------------------------------

    /// The three-way disagreement case the item's acceptance names
    /// explicitly: a profile sets one value, `extra` sets a DIFFERENT value
    /// for the SAME key, and a third key is set by neither -- `extra` wins
    /// on the shared key, and the caller's own default (not this function's
    /// concern) is what would fill the unset one.
    #[test]
    fn extra_wins_over_profile_on_a_shared_key_profile_fills_what_extra_does_not_set() {
        let profile = ProfileBundle {
            id: "preset".to_string(),
            fields: BTreeMap::from([
                (
                    "anthropic_version".to_string(),
                    serde_json::json!("2023-01-01"),
                ),
                (
                    "headers".to_string(),
                    serde_json::json!({"x-from-profile": "1"}),
                ),
            ]),
        };
        let extra = BTreeMap::from([(
            "anthropic_version".to_string(),
            serde_json::json!("2024-06-01"),
        )]);

        let merged = apply_precedence(Some(&profile), &extra);

        assert_eq!(
            merged.get("anthropic_version"),
            Some(&serde_json::json!("2024-06-01")),
            "explicit extra must win over the profile's value for the same key"
        );
        assert_eq!(
            merged.get("headers"),
            Some(&serde_json::json!({"x-from-profile": "1"})),
            "a key extra never set must still come from the profile"
        );
    }

    #[test]
    fn apply_precedence_with_no_profile_is_extra_alone() {
        let extra = BTreeMap::from([("k".to_string(), serde_json::json!("v"))]);
        let merged = apply_precedence(None, &extra);
        assert_eq!(merged, extra);
    }
}
