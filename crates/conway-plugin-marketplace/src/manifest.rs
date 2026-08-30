//! The marketplace wire shape and [`fetch_marketplace`], the "browse" half
//! of this crate's acceptance criterion 1.
//!
//! # Two manifest shapes, not one -- board item `01M0Y6RYZA94BK6YXJ7X8TNEGR`
//!
//! **This module's own doc used to claim "a marketplace manifest is a
//! format THIS item defines, so the ordinary strict rule applies." That
//! premise was false, and it is the reason the first operator to run
//! `/plugin install` against a real, published marketplace got a
//! confusing refusal instead of an install.** A marketplace manifest is
//! Claude Code's format, not conway's -- conway reads it, exactly the
//! relationship `conway_plugin_claude` already has with `.claude-plugin/
//! plugin.json`/`.mcp.json`/`commands/*.md`, and that crate's own module
//! doc states the correct rule plainly: a file this project does not
//! define the schema of is parsed permissively, an unrecognized field is
//! simply never looked at, and a `#[serde(deny_unknown_fields)]` struct is
//! reserved for a format conway itself owns. This module now follows that
//! rule instead of arguing against it.
//!
//! A `plugins[]` entry comes in two shapes, both accepted:
//!
//! ```json
//! {
//!   "name": "acme-marketplace",
//!   "owner": { "name": "Acme Corp" },
//!   "metadata": { "description": "Acme's plugins", "version": "1.0.0" },
//!   "plugins": [
//!     {
//!       "id": "acme-tools",
//!       "name": "Acme Tools",
//!       "description": "Search and lookup tools for Acme's internal index",
//!       "version": "1.0.0",
//!       "files": {
//!         ".claude-plugin/plugin.json": "https://example.com/acme-tools/plugin.json",
//!         ".mcp.json": "https://example.com/acme-tools/mcp.json"
//!       }
//!     },
//!     {
//!       "name": "beepboop",
//!       "source": { "source": "git-subdir", "url": "https://github.com/devnill/beepboop", "path": "plugin" },
//!       "description": "Plays sounds on hook events",
//!       "version": "1.4.0"
//!     }
//!   ]
//! }
//! ```
//!
//! The first entry is conway's own **files-map** shape: a per-file
//! `{relpath -> URL}` map this crate fetches over HTTP one file at a time
//! (see this module's own "no archive" argument, below, and
//! `install.rs`'s own doc for the path-safety mechanics). It is identified
//! by `id`, and is **kept** -- not the only shape understood, never
//! deleted -- because it is what lets a conway-native marketplace exist at
//! all without a git remote of its own.
//!
//! The second is the **real, published Claude Code marketplace** shape
//! (verified against `https://raw.githubusercontent.com/devnill/
//! claude-marketplace/HEAD/.claude-plugin/marketplace.json`, fetched
//! 2026-08-26; the exact bytes are the committed fixture
//! `tests/fixtures/claude-code-marketplace.json`). It is identified by
//! `name` (there is no `id`), and instead of `files` names a [`PluginSource`]
//! -- `git-subdir` (a repository URL plus a subdirectory) or `github` (an
//! `owner/repo` pair) -- fetched by invoking the SYSTEM `git` binary
//! (`crate::git_source`), never a git library: see `Cargo.toml`'s own doc
//! for why a crate never entered this workspace's lock for this. The
//! top-level `owner`/`metadata` objects are read (`MarketplaceOwner`/
//! `MarketplaceMetadata`) but not required, and neither is
//! `#[serde(deny_unknown_fields)]` -- any field of either object beyond
//! what this crate reads is simply ignored, matching the same posture.
//!
//! [`MarketplacePluginEntry::identity`] resolves either shape to the one
//! string an operator names when installing/uninstalling: `id` when
//! present, `name` otherwise. An entry with neither is a manifest this
//! crate cannot use at all (`MarketplaceError::MissingIdentity`).
//!
//! # No archive support -- a narrower surface, not an absent one
//!
//! Neither shape can name a single archive URL (a `.tar.gz`/`.zip`). This
//! is unchanged by adding a git fetcher: `git2`/`tar`/`zip` are still none
//! of them in this workspace's lock (`Cargo.toml`'s own doc, amended for
//! this item's ruling, has the full argument), and a symlink inside an
//! extracted archive pointing outside the extraction root is a real safety
//! surface a first cut should not take on casually. A [`PluginSource`]
//! naming any kind other than `git-subdir`/`github` parses successfully
//! (so browsing a marketplace that lists one still works) but refuses BY
//! NAME the moment an install is attempted (`MarketplaceError::
//! UnsupportedSourceKind`) -- `crate::git_source`'s own doc has the fetch
//! side of this.
//!
//! `files`-shaped entries keep their own "no archive, one file at a time"
//! argument unchanged: fetching each declared file individually means
//! there is no archive-extraction step for that shape either, and
//! therefore no symlink-in-an-archive class of attack to defend against at
//! all for it. A git checkout is a DIFFERENT surface with its own hazard
//! (a checkout can itself contain a symlink) -- `crate::git_source`'s own
//! doc states how that is validated before anything is installed.
//!
//! # A third shape found within hours -- `source` as a plain string
//!
//! Board item `01M1A9J9C9YRH3YPTGD335HZPZ`: the operator's very next
//! attempt, against ideate's own real marketplace, hit a THIRD shape this
//! module's own `git-subdir`/`github`-object model above did not cover at
//! all -- `"source": "./"`, a bare relative path meaning "this repository
//! is the plugin". [`PluginSource::RelativePath`] is that shape (that
//! enum's own doc has the full argument for why this is likely the
//! COMMONEST real-world case, not an edge one). Verified against ideate's
//! own manifest (`https://raw.githubusercontent.com/ideate-ai/ideate/HEAD/
//! .claude-plugin/marketplace.json`, fetched 2026-08-30; the exact bytes
//! are the committed fixture `tests/fixtures/ideate-marketplace.json`).
//!
//! **Other Claude Code manifest shapes checked for and NOT found** (so this
//! stays a documented finding rather than an unstated assumption): the
//! `files`/`directory` `source` kinds Claude Code's own schema documents
//! alongside `git-subdir`/`github` were already representable before this
//! item (`PluginSource::Unsupported`, refused by name only at install
//! time -- see `docs/migrating-from-claude-code.md`'s own worked
//! `extraKnownMarketplaces` example, which uses `"source": "directory"` for
//! a local path); no `plugins[]` entry combining `id` AND `source`, or
//! `name` AND `files`, was found in either real manifest this crate has
//! now ingested; neither real manifest nests a marketplace inside another
//! marketplace's own `plugins[]` array. A THIRD real, published manifest
//! was not located to check against beyond the two already fetched.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::error::MarketplaceError;

/// Safety cap on a marketplace manifest response's own byte size -- large
/// enough for a marketplace listing hundreds of plugins with real
/// descriptions, small enough that a malicious or broken marketplace
/// cannot make this crate buffer an unbounded response into memory.
pub const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;

/// One marketplace: a name, an optional description, every plugin it
/// lists, and (real Claude Code manifests only) an `owner`/`metadata`
/// object this crate reads but does not require -- see this module's own
/// doc for why `owner`/`metadata` are modeled explicitly while everything
/// INSIDE either is read permissively.
///
/// Still `#[serde(deny_unknown_fields)]` at this one level, deliberately:
/// every top-level field a real, published Claude Code manifest carries
/// (`name`, `description`, `owner`, `metadata`, `plugins`) now has a home,
/// so a field still unrecognized here is far more likely a typo in a
/// conway-native marketplace author's own document than an unmodeled
/// corner of Claude Code's schema -- catching that typo is worth keeping.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketplaceManifest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub owner: Option<MarketplaceOwner>,
    #[serde(default)]
    pub metadata: Option<MarketplaceMetadata>,
    pub plugins: Vec<MarketplacePluginEntry>,
}

impl MarketplaceManifest {
    /// Looks up one plugin by its [`MarketplacePluginEntry::identity`] --
    /// the "install" half's first step, after "browse" (fetching the
    /// manifest) has already happened. Returns a typed, named error rather
    /// than `None`, since every caller of this method already knows the
    /// marketplace's own URL to name in the message.
    pub fn find<'a>(
        &'a self,
        marketplace_url: &str,
        plugin_id: &str,
    ) -> Result<&'a MarketplacePluginEntry, MarketplaceError> {
        self.plugins
            .iter()
            .find(|p| p.identity() == Some(plugin_id))
            .ok_or_else(|| MarketplaceError::PluginNotFound {
                marketplace_url: marketplace_url.to_string(),
                plugin_id: plugin_id.to_string(),
            })
    }
}

/// A real Claude Code marketplace's top-level `owner` object. `name` is the
/// only field this crate has ever observed or reads; any other field an
/// owner object carries is simply never looked at -- this struct is NOT
/// `#[serde(deny_unknown_fields)]` (this module's own doc: foreign input,
/// read permissively).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct MarketplaceOwner {
    #[serde(default)]
    pub name: String,
}

/// A real Claude Code marketplace's top-level `metadata` object -- same
/// posture as [`MarketplaceOwner`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct MarketplaceMetadata {
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
}

/// One `plugins[]` entry -- everything an operator sees before consenting
/// to install (determine-first Q2: what conway shows before the write),
/// plus either the `files` map or the [`PluginSource`] [`crate::install::
/// install_plugin`] actually fetches from.
///
/// **Deliberately NOT `#[serde(deny_unknown_fields)]`**, unlike every other
/// struct in this module -- this is the one place this module's "foreign
/// format, read permissively" rule (this module's own doc) actually bites,
/// because a real Claude Code entry and a conway-native entry are two
/// different shapes sharing one Rust type. Every field is `#[serde(default)]`
/// so either shape parses; [`Self::identity`]/[`crate::install::
/// install_entry`] enforce, AFTER parsing, that an entry actually has
/// enough to be identified and installed -- serde alone cannot express "an
/// `id`+`files` XOR a `name`+`source`" constraint declaratively, and a typed
/// post-parse check reads more honestly than a hand-rolled `Deserialize`
/// impl would for a plain field-presence rule. The traded-off typo
/// protection this loses for a conway-native marketplace author's own
/// entry (a misspelled `files` key would now be silently ignored rather
/// than refused) is accepted deliberately: the alternative is two
/// completely separate entry types with no shared handling anywhere this
/// crate or its callers already have.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct MarketplacePluginEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    /// A real Claude Code entry's fetch instructions -- `git-subdir`/
    /// `github` fetched via `crate::git_source`, anything else refused BY
    /// NAME at install time (`MarketplaceError::UnsupportedSourceKind`).
    /// `None` for a conway-native, files-map entry.
    #[serde(default)]
    pub source: Option<PluginSource>,
    /// Relative path inside the installed plugin directory -> the URL this
    /// crate fetches its bytes from. See this module's own doc for why a
    /// per-file map, not a single archive URL. Empty for a real Claude Code
    /// entry, which names [`Self::source`] instead.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
}

impl MarketplacePluginEntry {
    /// The one string an operator names to install/uninstall this entry:
    /// `id` when a conway-native marketplace set one, `name` otherwise (the
    /// only identity a real Claude Code entry has). `None` when an entry
    /// carries neither -- a manifest this crate cannot use at all, however
    /// it otherwise parsed.
    pub fn identity(&self) -> Option<&str> {
        if !self.id.is_empty() {
            Some(self.id.as_str())
        } else if !self.name.is_empty() {
            Some(self.name.as_str())
        } else {
            None
        }
    }
}

/// A real Claude Code `plugins[].source` -- board item
/// `01M0Y6RYZA94BK6YXJ7X8TNEGR`, layer 4, widened by `01M1A9J9C9YRH3YPTGD335HZPZ`
/// (defect 1) to also accept the plain-STRING shape. Custom [`Deserialize`]
/// rather than `#[derive]` with `#[serde(tag = "source")]`: a derive-tagged
/// enum's "unknown variant" case is a hard parse error, which would make
/// browsing a marketplace fail outright the moment it lists ANY source kind
/// this crate does not fetch (an archive-requiring kind, or one published
/// after this was written) -- exactly the layer-4/layer-5 "refuse by name,
/// but only when actually asked to install it" split this item's ruling
/// draws. [`Self::Unsupported`] instead carries the kind's own name forward
/// so this crate's own `git_source` module can name it in a refusal,
/// without ever failing the PARSE.
///
/// # Defect 1: `source` may be a plain string, not only an object
///
/// ideate's own real, published manifest (`.claude-plugin/marketplace.json`
/// at `https://github.com/ideate-ai/ideate`, fetched 2026-08-30, committed
/// verbatim as `tests/fixtures/ideate-marketplace.json`) names
/// `"source": "./"` for its own `ideate` entry -- a relative path meaning
/// "this repository IS the plugin", not an object with an inner `source`
/// discriminator field at all. The custom [`Deserialize`] impl below used
/// to call `value.get("source")` unconditionally, which -- given a JSON
/// *string* rather than an object -- looked for a `source` field INSIDE
/// the string and reported `missing field `source``, a confusing error
/// about the very value it was trying to read. [`Self::RelativePath`] is
/// this shape; before this crate can fetch it, [`crate::install::
/// install_entry`] resolves it against the REPOSITORY the marketplace
/// manifest itself was reached through (`crate::git_source`'s own
/// resolution, using `github_repo_from_url` below) -- a bare relative path
/// alone names no git remote of its own.
///
/// This is likely the COMMONEST real-world shape, not an edge case: a
/// repository that publishes a Claude Code marketplace listing only
/// itself has no reason to name a `git-subdir`/`github` object pointing
/// back at its own clone URL when a bare `"./"` says the same thing more
/// simply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginSource {
    /// `{"source": "git-subdir", "url": "...", "path": "..."}` -- a git
    /// repository plus the subdirectory inside it that is this plugin's own
    /// root.
    GitSubdir { url: String, path: String },
    /// `{"source": "github", "repo": "owner/repo"}` -- a whole GitHub
    /// repository is this plugin's own root.
    Github { repo: String },
    /// A plain JSON string, e.g. `"./"` or `"plugin/subdir"` -- the
    /// marketplace's OWN repository is this plugin's root (or contains it
    /// at the named relative path). See this enum's own doc, "Defect 1".
    /// Resolved against the marketplace's own repository at install time
    /// (`crate::git_source`'s own `clone_url`/`subdir`); a marketplace this
    /// crate cannot trace back to a git repository at all (e.g. reached via
    /// an arbitrary HTTP host with no known git remote) refuses this
    /// resolution BY NAME (`MarketplaceError::UnresolvableRelativeSource`)
    /// rather than guessing one.
    RelativePath { path: String },
    /// Any other declared `source` value -- most plausibly one of the
    /// archive-requiring kinds this item's ruling explicitly scopes out
    /// (`Cargo.toml`'s own doc: no `tar`/`zip` extraction dependency).
    /// Carries the kind's own name, verbatim, so a refusal can name it.
    Unsupported { kind: String },
}

impl<'de> Deserialize<'de> for PluginSource {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        // Checked BEFORE `value.get("source")` -- a plain string has no
        // fields to `.get` at all, and asking it for one is exactly the
        // defect this enum's own doc describes ("Defect 1"). A string
        // value is never ambiguous with an object, so this check can never
        // shadow the object-shaped match below.
        if let Some(path) = value.as_str() {
            return Ok(PluginSource::RelativePath {
                path: path.to_string(),
            });
        }
        let kind = value
            .get("source")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| serde::de::Error::missing_field("source"))?
            .to_string();
        match kind.as_str() {
            "git-subdir" => Ok(PluginSource::GitSubdir {
                url: required_str_field(&value, "url")?,
                path: required_str_field(&value, "path")?,
            }),
            "github" => Ok(PluginSource::Github {
                repo: required_str_field(&value, "repo")?,
            }),
            _ => Ok(PluginSource::Unsupported { kind }),
        }
    }
}

/// Pulls a required string field out of an already-decoded `source` object
/// -- shared by every known [`PluginSource`] variant above. A missing or
/// non-string field is a normal `serde` `missing_field` error, so a
/// `git-subdir` entry that forgot its own `path` is reported exactly like
/// any other malformed manifest, not a panic.
fn required_str_field<E: serde::de::Error>(
    value: &serde_json::Value,
    field: &'static str,
) -> Result<String, E> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| serde::de::Error::missing_field(field))
}

/// Builds the one `reqwest::Client` this crate's public functions each use
/// -- a bounded total-request timeout so "no network" (a host that never
/// answers, not just one that refuses the connection immediately) is an
/// ordinary, clearly-reported failure rather than a hang, per this item's
/// own "offline ... must never hang" requirement. Never constructed once
/// and reused across calls: each public entry point in this crate builds
/// its own, matching the workspace's `HttpClient::with_timeout` precedent
/// (`conway-plugin-backends/src/http.rs`) of a cheap, short-lived client
/// per logical operation rather than a long-lived shared one this crate
/// would need to manage the lifetime of.
pub(crate) fn client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
}

/// Fetches and parses the marketplace manifest at `url` -- the "browse" the
/// spec names: an operator (or the install path, internally) sees every
/// plugin a marketplace lists before anything is written to disk.
///
/// Every failure mode is a named [`MarketplaceError`] variant, never a
/// panic and never an unbounded read: a network failure (DNS,
/// connection refused, timeout) is [`MarketplaceError::Network`]; a
/// non-2xx response is [`MarketplaceError::Http`]; a response exceeding
/// [`MAX_MANIFEST_BYTES`] is [`MarketplaceError::ResponseTooLarge`]; a 2xx
/// response whose body is not valid JSON or is missing a required field is
/// [`MarketplaceError::MalformedManifest`].
pub async fn fetch_marketplace(url: &str) -> Result<MarketplaceManifest, MarketplaceError> {
    let effective_url = resolve_marketplace_url(url)?;
    let client = client().map_err(|source| MarketplaceError::Network {
        url: effective_url.clone(),
        source,
    })?;
    let bytes = fetch_bytes(&client, &effective_url, MAX_MANIFEST_BYTES).await?;
    if looks_like_markup(&bytes) {
        return Err(MarketplaceError::NotAManifestUrl {
            url: effective_url.clone(),
            hint: manifest_url_hint(&effective_url),
        });
    }
    serde_json::from_slice(&bytes).map_err(|source| MarketplaceError::MalformedManifest {
        url: effective_url,
        message: source.to_string(),
    })
}

/// Resolves what [`fetch_marketplace`] should actually GET for `url` --
/// board item `01M1A9J9C9YRH3YPTGD335HZPZ`, defects 2 and 3 together, since
/// both are the same root cause: the operator named a GitHub REPOSITORY,
/// the same thing Claude Code's own `/plugin install`/`/plugin marketplace
/// add` accept, not a manifest DOCUMENT, and conway must resolve that
/// itself rather than fail or leak `reqwest`'s own error text.
///
/// - **Defect 3** -- a bare `https://github.com/<owner>/<repo>` repository
///   URL (no deeper path: `/tree/...`, `/blob/...`, and similar are left
///   unresolved, since the manifest's location is not mechanically
///   derivable from those) is expanded to the raw-content URL of the
///   manifest Claude Code itself keeps at that repository's own root
///   (`.claude-plugin/marketplace.json`). Board item
///   `01M0Y6RYZA94BK6YXJ7X8TNEGR` was supposed to make hunting for that URL
///   unnecessary and only wired the git-fetch half
///   (`crate::git_source::clone_url`, for a `source` INSIDE an
///   already-parsed manifest) -- this is the top-level fetch's own missing
///   half, finally wired in. **Once this resolves, [`manifest_url_hint`]'s
///   "usually at ..." suggestion never fires for this exact shape**: this
///   function already reached the resolved URL directly, so the "operator
///   sees a repository page, must hunt for the raw URL themselves"
///   experience this replaces does not happen for it at all.
/// - **Defect 2** -- a bare `<owner>/<repo>` GitHub shorthand, Claude
///   Code's own `/plugin marketplace add owner/repo` shape, is expanded the
///   identical way (via `https://github.com/<owner>/<repo>` first). This
///   previously reached `reqwest`'s own request builder as a malformed,
///   non-absolute URL and surfaced literally as **"builder error"** --
///   `reqwest::Error`'s own internal `Display`, an implementation detail no
///   operator should ever see. `github_repo_from_url` recognizes the
///   shorthand and expands it BEFORE any request is attempted, so this
///   never reaches `reqwest` at all for the reported shape.
/// - Anything already an absolute `http(s)://` URL is used completely
///   unchanged -- the overwhelmingly common case, a manifest document named
///   directly.
/// - Anything else is refused before a single byte is requested:
///   [`MarketplaceError::InvalidUrl`], naming exactly what conway accepts,
///   rather than reaching `reqwest` with a string it cannot build a
///   request from at all. [`fetch_bytes`] catches the same failure mode a
///   second time (`reqwest::Error::is_builder`), as defense in depth for
///   any input shape this function does not special-case (a per-file URL a
///   manifest itself declares, say) -- see that function's own doc.
pub(crate) fn resolve_marketplace_url(url: &str) -> Result<String, MarketplaceError> {
    if let Some((owner, repo)) = github_repo_root_url(url).or_else(|| github_shorthand_repo(url)) {
        return Ok(format!(
            "https://raw.githubusercontent.com/{owner}/{repo}/HEAD/.claude-plugin/marketplace.json"
        ));
    }
    if url.starts_with("https://") || url.starts_with("http://") {
        return Ok(url.to_string());
    }
    Err(MarketplaceError::InvalidUrl {
        url: url.to_string(),
    })
}

/// `<owner>/<repo>`, and nothing more -- the shared primitive behind every
/// "does this string name exactly a GitHub owner/repo pair" check below:
/// [`github_repo_root_url`] (after stripping a `github.com/` prefix),
/// [`github_shorthand_repo`] (no prefix to strip at all), and
/// [`github_raw_content_repo`] (after stripping a
/// `raw.githubusercontent.com/` prefix and an additional ref segment).
/// Requires EXACTLY two non-empty segments once a trailing `/` is
/// trimmed -- a third segment (`/tree/main`, `/blob/...`, a shorthand
/// typo'd with an extra `/`) means `rest` names something more specific
/// than a bare repository, which none of this function's callers can
/// resolve a manifest location from.
fn bare_repo_pair(rest: &str, allow_git_suffix: bool) -> Option<(String, String)> {
    let rest = rest.trim_end_matches('/');
    let mut parts = rest.split('/');
    let owner = parts.next()?;
    let repo_raw = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if owner.is_empty() || repo_raw.is_empty() {
        return None;
    }
    let repo = if allow_git_suffix {
        repo_raw.strip_suffix(".git").unwrap_or(repo_raw)
    } else {
        repo_raw
    };
    Some((owner.to_string(), repo.to_string()))
}

/// `https://github.com/<owner>/<repo>` (optional trailing `/` and/or
/// `.git`), and nothing deeper -- shared by [`resolve_marketplace_url`]'s
/// own "the operator named a bare repository" case and [`manifest_url_hint`]'s
/// fallback suggestion.
fn github_repo_root_url(url: &str) -> Option<(String, String)> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    bare_repo_pair(rest, true)
}

/// A no-scheme `<owner>/<repo>` shorthand -- Claude Code's own `/plugin
/// marketplace add owner/repo` shape (board item's defect 2). Refuses
/// anything containing `://` (an absolute URL naming a different host
/// entirely, never a shorthand) or starting with `.`/`/` (a relative or
/// absolute filesystem-shaped path, not a repository name) before even
/// trying to split it -- a wrong guess here is worse than none, exactly
/// [`manifest_url_hint`]'s own long-standing "a wrong suggestion is worse
/// than none" argument applied to resolution rather than a suggestion.
fn github_shorthand_repo(url: &str) -> Option<(String, String)> {
    if url.contains("://") || url.starts_with('.') || url.starts_with('/') {
        return None;
    }
    bare_repo_pair(url, false)
}

/// `https://raw.githubusercontent.com/<owner>/<repo>/<ref>/...` -- a raw
/// content URL already pointing INSIDE the repository at some ref, used
/// only by [`github_repo_from_url`] (never by [`resolve_marketplace_url`]
/// itself: a raw-content URL is already exactly the kind of URL that
/// function's job is to PRODUCE, so re-deriving and rebuilding one from it
/// would silently discard whatever ref/path the operator actually named).
fn github_raw_content_repo(url: &str) -> Option<(String, String)> {
    let rest = url
        .strip_prefix("https://raw.githubusercontent.com/")
        .or_else(|| url.strip_prefix("http://raw.githubusercontent.com/"))?;
    let mut parts = rest.splitn(3, '/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    parts.next()?; // a ref segment must follow, or this URL never pointed
                   // INSIDE a repository to begin with.
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

/// Any of the three shapes that name a GitHub repository -- board item
/// `01M1A9J9C9YRH3YPTGD335HZPZ`'s shared parser (P-14: one implementation),
/// used by `crate::git_source`'s own resolution of a
/// [`PluginSource::RelativePath`] at install time: whichever of the three
/// shapes the operator's own `marketplace_url` happens to be, this answers
/// "what repository is this install relative to" uniformly. Deliberately
/// wider than [`resolve_marketplace_url`]'s own matching (which excludes
/// the raw-content shape for the reason [`github_raw_content_repo`]'s own
/// doc states) -- that function answers "what should conway fetch", this
/// one answers "what repository is this", a different question the
/// raw-content shape has a perfectly good answer to.
pub(crate) fn github_repo_from_url(url: &str) -> Option<(String, String)> {
    github_repo_root_url(url)
        .or_else(|| github_shorthand_repo(url))
        .or_else(|| github_raw_content_repo(url))
}

/// Is this response body markup rather than JSON?
///
/// Deliberately a *shape* check on the body and not a `Content-Type` check:
/// a `Content-Type` header is trivially absent or wrong on a static host,
/// and this test only has to be right about the case it exists for -- a
/// human pasted a page URL and got a page back. Leading whitespace is
/// skipped, then a single byte decides: a JSON document that is a
/// marketplace manifest always begins `{`, and no JSON value of any kind
/// begins `<`.
///
/// This is intentionally NOT a general "is it HTML" test. It does not sniff
/// for `<!doctype`, `<html`, or anything else specific, because it does not
/// need to: the question being answered is only "is this markup instead of
/// the JSON we asked for", and one byte settles that without this crate
/// growing an opinion about markup dialects it never parses.
fn looks_like_markup(bytes: &[u8]) -> bool {
    matches!(bytes.iter().find(|b| !b.is_ascii_whitespace()), Some(&b'<'))
}

/// Builds the "try this URL instead" clause for
/// [`MarketplaceError::NotAManifestUrl`], or an empty string when no
/// suggestion can be derived honestly.
///
/// **Only GitHub repository URLs get a suggestion**, because GitHub is the
/// one host where the raw-content URL for a path inside a repository is
/// mechanically derivable from the repository URL. For anything else this
/// returns `""` and the error says what conway wanted without pretending to
/// know where the operator's manifest lives -- a wrong suggestion is worse
/// than none, since an operator who follows it gets a second, more
/// confusing failure.
///
/// The suggested path is `.claude-plugin/marketplace.json` — where Claude
/// Code keeps a marketplace manifest, and therefore where an operator who
/// typed a repository URL most likely has one. Board item
/// `01M0Y6RYZA94BK6YXJ7X8TNEGR` ruled layers 2-4: a real Claude Code
/// manifest now parses (this module's own doc) and its `git-subdir`/
/// `github` sources now fetch (`crate::git_source`) -- so this suggestion
/// is no longer a URL that leads to a second, different refusal, only a
/// GUESS at where the document lives (this crate never fetches the
/// suggested URL itself to confirm it exists).
fn manifest_url_hint(url: &str) -> String {
    let Some((owner, repo)) = github_repo_root_url(url) else {
        return String::new();
    };
    format!(
        " -- if this is a Claude Code marketplace repository, its manifest is usually at \
         https://raw.githubusercontent.com/{owner}/{repo}/HEAD/.claude-plugin/marketplace.json"
    )
}

/// Shared by [`fetch_marketplace`] and [`crate::install::install_plugin`]
/// (each per-file fetch): GETs `url`, refuses a non-2xx status, and refuses
/// (before AND after the read) a body exceeding `cap` bytes -- checked
/// against `Content-Length` when present so an oversized response can be
/// refused before this crate reads a single byte of it, and checked again
/// against the actual decoded length so a server that omits or lies about
/// `Content-Length` cannot bypass the cap either.
///
/// **Never leaks `reqwest`'s own "builder error" text** (board item
/// `01M1A9J9C9YRH3YPTGD335HZPZ`, defect 2): `client.get(url)` defers URL
/// parsing to `.send()`, so a `url` `reqwest` cannot turn into a request at
/// all (not absolute, an unsupported scheme, ...) fails HERE with
/// `reqwest::Error::is_builder() == true` -- caught explicitly and mapped
/// to [`MarketplaceError::InvalidUrl`], a named, typed failure, rather than
/// falling into [`MarketplaceError::Network`] and rendering `reqwest`'s own
/// internal `Display` (literally the string "builder error: ...") to an
/// operator. [`resolve_marketplace_url`] already prevents the reported
/// shape (an `owner/repo` shorthand used as `fetch_marketplace`'s own top-
/// level URL) from ever reaching this function malformed; this check is
/// defense in depth for anything else that still could -- a per-file URL a
/// marketplace's own `files` map declares, for instance.
pub(crate) async fn fetch_bytes(
    client: &reqwest::Client,
    url: &str,
    cap: u64,
) -> Result<Vec<u8>, MarketplaceError> {
    let response = client.get(url).send().await.map_err(|source| {
        if source.is_builder() {
            MarketplaceError::InvalidUrl {
                url: url.to_string(),
            }
        } else {
            MarketplaceError::Network {
                url: url.to_string(),
                source,
            }
        }
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(MarketplaceError::Http {
            url: url.to_string(),
            status: status.as_u16(),
        });
    }
    if let Some(len) = response.content_length() {
        if len > cap {
            return Err(MarketplaceError::ResponseTooLarge {
                url: url.to_string(),
                actual: len,
                limit: cap,
            });
        }
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|source| MarketplaceError::Network {
            url: url.to_string(),
            source,
        })?;
    if bytes.len() as u64 > cap {
        return Err(MarketplaceError::ResponseTooLarge {
            url: url.to_string(),
            actual: bytes.len() as u64,
            limit: cap,
        });
    }
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    /// Acceptance 5, case 1: offline. Nothing is listening on this address
    /// (a `MockServer` bound then immediately dropped still leaves the port
    /// free, but simpler and equally decisive: an unroutable TEST-NET-1
    /// address per RFC 5737 refuses/times out deterministically without
    /// depending on port-reuse timing) -- a clear, typed error, never a
    /// hang and never a panic.
    #[tokio::test]
    async fn offline_is_a_clear_typed_error_not_a_hang() {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            fetch_marketplace("http://127.0.0.1:1/marketplace.json"),
        )
        .await
        .expect("must not hang past the 5s test timeout");
        let err = result.expect_err("nothing is listening on this port");
        assert_eq!(err.kind(), "network");
    }

    /// Acceptance 5, case 2: a bad URL -- a real host that answers, just
    /// not with the marketplace.
    #[tokio::test]
    async fn a_404_is_reported_as_a_named_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/marketplace.json"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let url = format!("{}/marketplace.json", server.uri());
        let err = fetch_marketplace(&url).await.expect_err("404");
        assert_eq!(err.kind(), "http");
        assert!(err.to_string().contains("404"), "{err}");
    }

    /// Acceptance 5, case 3: a malformed marketplace response -- valid
    /// HTTP, invalid JSON.
    #[tokio::test]
    async fn invalid_json_is_a_malformed_manifest_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/marketplace.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{ this is not json"))
            .mount(&server)
            .await;

        let url = format!("{}/marketplace.json", server.uri());
        let err = fetch_marketplace(&url).await.expect_err("invalid json");
        assert_eq!(err.kind(), "malformed_manifest");
    }

    /// Acceptance 5, case 3, second shape: valid JSON, wrong shape (missing
    /// the required `plugins` field) -- `deny_unknown_fields` plus no
    /// `#[serde(default)]` on `plugins` means this is refused, not silently
    /// defaulted to an empty marketplace.
    #[tokio::test]
    async fn valid_json_missing_a_required_field_is_a_malformed_manifest_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/marketplace.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"name": "acme"}"#))
            .mount(&server)
            .await;

        let url = format!("{}/marketplace.json", server.uri());
        let err = fetch_marketplace(&url)
            .await
            .expect_err("missing plugins field");
        assert_eq!(err.kind(), "malformed_manifest");
    }

    /// The success path: browsing a real marketplace lists every plugin,
    /// with the fields an operator would review before installing.
    #[tokio::test]
    async fn a_well_formed_marketplace_lists_every_plugin() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/marketplace.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{
                    "name": "acme-marketplace",
                    "description": "Acme's plugins",
                    "plugins": [
                        {
                            "id": "acme-tools",
                            "name": "Acme Tools",
                            "description": "Search Acme's index",
                            "version": "1.0.0",
                            "files": {
                                ".claude-plugin/plugin.json": "https://example.com/plugin.json",
                                ".mcp.json": "https://example.com/mcp.json"
                            }
                        }
                    ]
                }"#,
            ))
            .mount(&server)
            .await;

        let url = format!("{}/marketplace.json", server.uri());
        let manifest = fetch_marketplace(&url).await.expect("fetch");
        assert_eq!(manifest.name, "acme-marketplace");
        assert_eq!(manifest.plugins.len(), 1);
        let entry = manifest.find(&url, "acme-tools").expect("found");
        assert_eq!(entry.name, "Acme Tools");
        assert_eq!(entry.files.len(), 2);

        let missing = manifest.find(&url, "nope").expect_err("not listed");
        assert_eq!(missing.kind(), "plugin_not_found");
    }

    /// A response over the manifest size cap is refused before it is ever
    /// parsed -- proven with `Content-Length` present (the fast-path
    /// refusal, before any byte of the body is read).
    #[tokio::test]
    async fn an_oversized_manifest_is_refused_by_content_length() {
        let server = MockServer::start().await;
        let huge = "x".repeat((MAX_MANIFEST_BYTES + 1) as usize);
        Mock::given(method("GET"))
            .and(path("/marketplace.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(huge))
            .mount(&server)
            .await;

        let url = format!("{}/marketplace.json", server.uri());
        let err = fetch_marketplace(&url).await.expect_err("too large");
        assert_eq!(err.kind(), "response_too_large");
    }
}

/// Board item `01M0Y6RYZA94BK6YXJ7X8TNEGR`, layers 2-3: the real schema now
/// parses -- `owner`/`metadata` are tolerated, and a `name`+`source` entry
/// is accepted alongside an `id`+`files` one.
#[cfg(test)]
mod real_schema_tests {
    use super::*;

    /// A conway-native entry (`id`+`files`) and a real Claude Code entry
    /// (`name`+`source`) coexist in one manifest -- the shape neither
    /// format alone had to prove.
    #[test]
    fn owner_and_metadata_are_tolerated_and_both_entry_shapes_parse() {
        let manifest: MarketplaceManifest = serde_json::from_str(
            r#"{
                "name": "mixed-marketplace",
                "owner": { "name": "Acme Corp", "unexpected_owner_field": true },
                "metadata": { "description": "d", "version": "1.0.0", "unexpected_meta_field": 1 },
                "plugins": [
                    {
                        "id": "acme-tools",
                        "files": { "a.json": "https://example.com/a.json" }
                    },
                    {
                        "name": "beepboop",
                        "source": {
                            "source": "git-subdir",
                            "url": "https://github.com/devnill/beepboop",
                            "path": "plugin"
                        },
                        "description": "plays sounds",
                        "version": "1.4.0"
                    }
                ]
            }"#,
        )
        .expect("real schema, plus an unrecognized field inside owner/metadata, must parse");

        assert_eq!(manifest.owner.unwrap().name, "Acme Corp");
        assert_eq!(manifest.metadata.unwrap().version, "1.0.0");

        let native = &manifest.plugins[0];
        assert_eq!(native.identity(), Some("acme-tools"));
        assert!(native.source.is_none());

        let claude_code = &manifest.plugins[1];
        assert_eq!(claude_code.identity(), Some("beepboop"));
        assert!(claude_code.files.is_empty());
        assert_eq!(
            claude_code.source,
            Some(PluginSource::GitSubdir {
                url: "https://github.com/devnill/beepboop".to_string(),
                path: "plugin".to_string(),
            })
        );
    }

    /// An entry naming neither `id` nor `name` has no identity -- the
    /// post-parse check this module's own doc says serde cannot express
    /// declaratively.
    #[test]
    fn an_entry_with_neither_id_nor_name_has_no_identity() {
        let entry: MarketplacePluginEntry =
            serde_json::from_str(r#"{"description": "orphaned"}"#).expect("still parses");
        assert_eq!(entry.identity(), None);
    }

    /// [`PluginSource`]'s `github` shape.
    #[test]
    fn a_github_source_parses() {
        let entry: MarketplacePluginEntry = serde_json::from_str(
            r#"{"name": "ideate", "source": {"source": "github", "repo": "ideate-ai/ideate"}}"#,
        )
        .expect("github source parses");
        assert_eq!(
            entry.source,
            Some(PluginSource::Github {
                repo: "ideate-ai/ideate".to_string()
            })
        );
    }

    /// Board item's acceptance 4: a source kind requiring an archive (or
    /// any kind this crate simply does not know) still PARSES -- browsing a
    /// marketplace that lists one must not fail outright -- but is captured
    /// as [`PluginSource::Unsupported`], named, so a later install attempt
    /// can refuse it by name (see `git_source.rs`'s own tests for that
    /// refusal).
    #[test]
    fn an_unrecognized_source_kind_parses_as_unsupported_rather_than_failing() {
        let entry: MarketplacePluginEntry = serde_json::from_str(
            r#"{"name": "archived-thing", "source": {"source": "url", "url": "https://example.com/thing.tar.gz"}}"#,
        )
        .expect("an unrecognized source kind must not fail the parse");
        assert_eq!(
            entry.source,
            Some(PluginSource::Unsupported {
                kind: "url".to_string()
            })
        );
    }

    /// A `git-subdir` source missing its own required `path` is a normal,
    /// named parse failure -- never a panic.
    #[test]
    fn a_git_subdir_source_missing_a_required_field_is_a_clean_parse_error() {
        let err = serde_json::from_str::<MarketplacePluginEntry>(
            r#"{"name": "x", "source": {"source": "git-subdir", "url": "https://example.com/x"}}"#,
        )
        .expect_err("missing `path`");
        assert!(err.to_string().contains("path"), "{err}");
    }
}

/// Board item `01M0Y6RYZA94BK6YXJ7X8TNEGR`, layer 1: an operator who passes
/// a repository page gets told what conway wanted, not a `serde_json`
/// column reference to somebody else's markup.
#[cfg(test)]
mod not_a_manifest_url_tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// The exact reported failure, reproduced: a GitHub repository page
    /// answers 200 with HTML, and the operator used to see
    /// *"expected value at line 7 column 1"*.
    #[tokio::test]
    async fn a_repository_page_is_named_as_such_not_reported_as_bad_json() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/owner/repo"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<title>a repo</title>\n</head>\n",
                "text/html",
            ))
            .mount(&server)
            .await;

        let url = format!("{}/owner/repo", server.uri());
        let err = fetch_marketplace(&url)
            .await
            .expect_err("html is not a manifest");

        assert_eq!(err.kind(), "not_a_manifest_url", "got: {err}");
        // The operator must learn what conway wanted. Asserting on the
        // rendered message here (rather than only the kind) is deliberate:
        // this variant exists *for* its wording, so the wording is the
        // behaviour under test.
        let message = err.to_string();
        assert!(
            message.contains("not a marketplace manifest"),
            "message must name the actual problem, got: {message}"
        );
        assert!(
            !message.contains("line 7"),
            "must not leak a parse position into a URL diagnosis, got: {message}"
        );
    }

    /// Leading whitespace before the markup does not defeat the check.
    #[tokio::test]
    async fn markup_after_leading_whitespace_is_still_markup() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/p"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw("\n\n  \t<html></html>", "text/html"),
            )
            .mount(&server)
            .await;

        let err = fetch_marketplace(&format!("{}/p", server.uri()))
            .await
            .expect_err("still markup");
        assert_eq!(err.kind(), "not_a_manifest_url", "got: {err}");
    }

    /// **The distinction this whole variant has to preserve**: real JSON
    /// that conway's schema refuses is still a `malformed_manifest`. Layer
    /// 1's friendlier error must never swallow the deeper schema
    /// incompatibility -- see `tests/claude_code_manifest.rs`.
    #[tokio::test]
    async fn valid_json_of_the_wrong_shape_is_still_a_malformed_manifest() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/m.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(r#"{"unexpected": true}"#, "application/json"),
            )
            .mount(&server)
            .await;

        let err = fetch_marketplace(&format!("{}/m.json", server.uri()))
            .await
            .expect_err("wrong shape");
        assert_eq!(err.kind(), "malformed_manifest", "got: {err}");
    }

    #[test]
    fn a_github_repository_url_gets_a_concrete_suggestion() {
        let hint = manifest_url_hint("https://github.com/devnill/claude-marketplace");
        assert!(
            hint.contains(
                "https://raw.githubusercontent.com/devnill/claude-marketplace/HEAD/.claude-plugin/marketplace.json"
            ),
            "got: {hint}"
        );
    }

    #[test]
    fn a_trailing_slash_does_not_defeat_the_suggestion() {
        assert_eq!(
            manifest_url_hint("https://github.com/devnill/claude-marketplace/"),
            manifest_url_hint("https://github.com/devnill/claude-marketplace"),
        );
    }

    /// A wrong suggestion is worse than none, so anything that is not
    /// recognisably an `owner/repo` pair on GitHub gets no suggestion at
    /// all -- including deeper GitHub paths, where the manifest location
    /// is not mechanically derivable.
    #[test]
    fn anything_not_a_bare_github_repo_gets_no_suggestion() {
        for url in [
            "https://example.com/some/marketplace",
            "https://github.com/devnill",
            "https://github.com/devnill/claude-marketplace/tree/main/plugins",
            "https://gitlab.com/devnill/claude-marketplace",
            "not a url at all",
        ] {
            assert_eq!(manifest_url_hint(url), "", "should not guess for {url}");
        }
    }

    #[test]
    fn markup_detection_does_not_fire_on_a_real_manifest_body() {
        assert!(!looks_like_markup(br#"{"name":"m","plugins":[]}"#));
        assert!(!looks_like_markup(b"  \n{}"));
        assert!(!looks_like_markup(b""));
    }
}

/// Board item `01M1A9J9C9YRH3YPTGD335HZPZ`, defects 2 and 3: resolving what
/// [`fetch_marketplace`] actually GETs for a bare repository URL or an
/// `owner/repo` shorthand, and never leaking `reqwest`'s own "builder
/// error" text for anything this crate cannot turn into a request.
#[cfg(test)]
mod resolve_marketplace_url_tests {
    use super::*;

    /// Defect 3, the exact reported shape: a bare repository URL resolves
    /// to the raw-content manifest URL Claude Code itself keeps at that
    /// repository's own root -- no operator hunting for it required.
    #[test]
    fn a_bare_github_repository_url_resolves_to_the_raw_manifest_url() {
        assert_eq!(
            resolve_marketplace_url("https://github.com/ideate-ai/ideate").unwrap(),
            "https://raw.githubusercontent.com/ideate-ai/ideate/HEAD/.claude-plugin/marketplace.json"
        );
    }

    /// A trailing slash or a `.git` suffix does not defeat resolution.
    #[test]
    fn a_trailing_slash_or_git_suffix_does_not_defeat_resolution() {
        let expected =
            "https://raw.githubusercontent.com/ideate-ai/ideate/HEAD/.claude-plugin/marketplace.json";
        assert_eq!(
            resolve_marketplace_url("https://github.com/ideate-ai/ideate/").unwrap(),
            expected
        );
        assert_eq!(
            resolve_marketplace_url("https://github.com/ideate-ai/ideate.git").unwrap(),
            expected
        );
    }

    /// Defect 2, the exact reported shape: a bare `owner/repo` shorthand
    /// (Claude Code's own `/plugin marketplace add owner/repo`) resolves
    /// the identical way -- **the discriminating observable**: before this
    /// fix, this exact string reached `reqwest`'s own request builder as a
    /// non-absolute URL and failed with `reqwest`'s own internal "builder
    /// error" text (`crate::error::MarketplaceError::Network`'s rendered
    /// `Display`); this asserts the NEW, resolved value instead of any
    /// error at all.
    #[test]
    fn an_owner_repo_shorthand_resolves_the_same_way_as_the_bare_repository_url() {
        assert_eq!(
            resolve_marketplace_url("ideate-ai/ideate").unwrap(),
            "https://raw.githubusercontent.com/ideate-ai/ideate/HEAD/.claude-plugin/marketplace.json"
        );
    }

    /// A deeper GitHub path (`/tree/...`) is not mechanically "the repo
    /// root" -- left unresolved, same as `manifest_url_hint`'s own refusal
    /// to guess for it.
    #[test]
    fn a_deeper_github_path_is_left_unresolved() {
        let url = "https://github.com/devnill/claude-marketplace/tree/main/plugins";
        assert_eq!(resolve_marketplace_url(url).unwrap(), url);
    }

    /// An already-absolute URL naming a manifest document directly --
    /// whether or not it happens to live on GitHub -- passes through
    /// completely unchanged. A raw-content URL matches this branch too:
    /// re-deriving one from `github_repo_from_url`'s raw-content parsing
    /// would silently discard whatever ref/path the operator actually
    /// named, which `resolve_marketplace_url`'s own doc states is exactly
    /// why it does not use that parser at all.
    #[test]
    fn an_already_absolute_url_is_used_unchanged() {
        for url in [
            "http://127.0.0.1:9999/marketplace.json",
            "https://example.com/some/marketplace.json",
            "https://raw.githubusercontent.com/devnill/beepboop/main/some/other/manifest.json",
        ] {
            assert_eq!(resolve_marketplace_url(url).unwrap(), url);
        }
    }

    /// Anything that is neither an absolute URL nor a recognizable
    /// shorthand is refused BEFORE a request is ever attempted -- named,
    /// never `reqwest`'s own "builder error" text.
    #[test]
    fn anything_else_is_refused_by_name_before_any_request() {
        for bad in [
            "not a valid marketplace string!!",
            "ftp://example.com/x",
            "./a/relative/path",
            "/an/absolute/path",
        ] {
            let err = resolve_marketplace_url(bad).expect_err(bad);
            assert_eq!(err.kind(), "invalid_url", "{bad}");
            assert!(
                !err.to_string().contains("builder error"),
                "must never leak reqwest's own error text: {err}"
            );
        }
    }

    /// [`fetch_bytes`]'s own defense-in-depth check: a URL that passes
    /// `resolve_marketplace_url` (it IS `http://`-prefixed) but is
    /// otherwise malformed enough that `reqwest` itself cannot parse it
    /// still never surfaces `reqwest`'s own "builder error" text -- proven
    /// through the real public entry point, not the private helper alone.
    #[tokio::test]
    async fn a_malformed_but_prefixed_url_still_never_leaks_builder_error_text() {
        let err = fetch_marketplace("http://[not-a-valid-host")
            .await
            .expect_err("an unparseable URL must not silently succeed");
        assert_eq!(err.kind(), "invalid_url", "{err}");
        assert!(
            !err.to_string().contains("builder error"),
            "must never leak reqwest's own error text: {err}"
        );
    }
}
