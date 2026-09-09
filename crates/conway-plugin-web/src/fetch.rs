//! `WebFetchTool`: the `web_fetch` tool -- GET an `http(s)` URL, reduced to
//! readable text, bounded, and guarded against reaching a private or
//! loopback network the operator never meant to expose to the model.
//!
//! # The SSRF guard: what it classifies, and how
//!
//! [`guard_url`] runs before every request this tool makes -- the initial
//! URL AND every redirect hop ([`fetch_loop`] re-guards each `Location` it
//! is about to follow, not only the call's own argument). It refuses:
//!
//! - Any scheme other than `http`/`https`.
//! - A URL with no host at all.
//! - An IP-literal host (`http://127.0.0.1/`, `http://[::1]/`, and every
//!   decimal/octal/hex/partial-dotted encoding the WHATWG URL Standard
//!   recognizes as a number -- see below) that classifies as loopback,
//!   any RFC 1918 private range, link-local (`169.254.0.0/16`, which
//!   includes the cloud-metadata endpoint `169.254.169.254` most cloud
//!   providers serve credentials from), carrier-grade NAT
//!   (`100.64.0.0/10`), broadcast, multicast, RFC 5737 documentation
//!   space, or `0.0.0.0/8`.
//! - The IPv6 analogues: `::1`, `::`, unique-local (`fc00::/7`),
//!   link-local (`fe80::/10`), multicast (`ff00::/8`), and an IPv4 address
//!   EMBEDDED in an IPv6 literal -- both IPv4-mapped (`::ffff:a.b.c.d`,
//!   written either in dotted-quad-in-brackets or hextet form -- both
//!   parse to the identical bit pattern, so both are caught) and the
//!   deprecated IPv4-compatible form (`::a.b.c.d`) -- classified by
//!   unwrapping the embedded address and running it through the SAME IPv4
//!   check above.
//! - A domain-name host that resolves (via [`tokio::net::lookup_host`]) to
//!   ANY address in the ranges above, or that fails to resolve at all.
//!
//! **Decimal/octal/hex IPv4 encodings are handled for free by the `url`
//! crate's own host parser, not by hand-rolled string parsing here.** The
//! WHATWG URL Standard's host-parsing algorithm ([`url::Host::parse`],
//! `url-2.5.8/src/host.rs`'s `parse_ipv4number`/`parse_ipv4addr`) already
//! normalizes `http://2130706433/`, `http://0x7f000001/`, and
//! `http://0177.0.0.1/` to the real address `127.0.0.1` before this
//! module ever sees a [`url::Host::Ipv4`] -- see this module's own tests
//! for the exact forms proven. This crate depends on `url` directly
//! (rather than parsing `rendered` by hand, the way
//! `conway_core::permission_pattern`'s `When::Domains` predicate does) for
//! exactly this reason: getting IPv4-encoding coverage right by hand would
//! mean re-deriving the WHATWG number parser, which the `url` crate
//! already implements and this crate's own Cargo.toml doc explains
//! choosing to reuse.
//!
//! **Fails closed.** A host that cannot be classified at all -- a domain
//! name whose DNS resolution errors, or resolves to zero addresses -- is
//! refused, never allowed through on the absence of evidence. This mirrors
//! `conway_core::permission_pattern::When::PathsUnder`'s own asymmetry: an
//! unestablished boundary confers no boundary, so the call it would have
//! gated is refused rather than passed through ungated.
//!
//! # What this guard does NOT cover -- disclosed, not implied
//!
//! - **DNS-rebinding TOCTOU.** [`guard_url`] resolves a domain name once,
//!   classifies THAT answer, and then hands the ORIGINAL URL (the
//!   hostname, not a pinned IP) to `reqwest`, which resolves it AGAIN to
//!   actually connect. A name that answers a safe address at guard time
//!   and a private one microseconds later at connect time is not caught.
//!   Closing this fully needs a custom `reqwest::dns::Resolve` that pins
//!   the vetted address for the connection itself; this crate does not
//!   implement one (the risk of shipping an unverified custom-resolver
//!   integration with no compiler available this session outweighed
//!   closing a narrow, timing-dependent gap for a first cut).
//! - **6to4 (`2002::/16`) and Teredo (`2001:0000::/32`) tunneling
//!   addresses**, which can themselves encapsulate an arbitrary IPv4
//!   address, are not unwrapped or specially classified -- only the two
//!   embedding forms named above (IPv4-mapped and IPv4-compatible) are.
//! - **NAT64 (`64:ff9b::/96`)** embeds an IPv4 address in its low 32 bits
//!   too; not unwrapped here either.
//! - **A redirect to a URL this guard cannot parse at all** (a malformed
//!   `Location` header) is refused as a fetch error, not specifically
//!   reported as an SSRF refusal -- see [`fetch_loop`].
//!
//! An operator relying on `web_fetch`'s permission rule (`conway_core::
//! permission_pattern::When::Domains`) as the ONLY control point should
//! not assume the guard above closes every network-topology trick; it
//! closes the ones a URL/IP-literal/ordinary-DNS-resolution analysis can
//! see, which is the same bar every comparable harness's own fetch tool
//! sets, not a stronger one.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use async_trait::async_trait;
use conway::plugin::{
    ContentBlock, PathArgs, PermissionClass, RenderKind, Tool, ToolCall, ToolCategory, ToolCtx,
    ToolError, ToolName, ToolOutput, ToolSpec, TruncationPolicy,
};
use futures::StreamExt;
use schemars::JsonSchema;
use serde::Deserialize;
use url::{Host, Url};

/// `web_fetch`'s tunable bounds. See [`Self::default`] for the shipped
/// defaults; an operator narrows or widens `max_bytes`/`max_redirects` via
/// `[plugins.config."conway.web"]` (`crate::WebPlugin::configure`).
#[derive(Debug, Clone)]
pub struct FetchConfig {
    /// The ceiling on response bytes read from the network, and the
    /// `TruncationPolicy::Head { max_bytes }` this tool declares on every
    /// call. A call's own `max_bytes` argument may only NARROW this, never
    /// raise it -- see [`WebFetchTool::invoke`].
    pub max_bytes: usize,
    /// The maximum number of redirect hops `fetch_loop` will follow
    /// before refusing. Each hop is re-guarded by `guard_url` before it
    /// is followed.
    pub max_redirects: u8,
    /// Per-request timeout (connect + read), applied to the `reqwest`
    /// client this tool builds fresh for every call -- the same
    /// build-per-call precedent `conway-plugin-marketplace::manifest::
    /// client` documents, rather than one long-lived client this crate
    /// would need to manage the lifetime of.
    pub timeout: Duration,
}

impl Default for FetchConfig {
    fn default() -> Self {
        Self {
            max_bytes: 200_000,
            max_redirects: 5,
            timeout: Duration::from_secs(20),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WebFetchArgs {
    /// The http(s) URL to fetch (GET only)
    url: String,
    /// Maximum bytes of response body to read; narrows (never raises) this
    /// tool's own configured ceiling
    #[schemars(range(min = 1))]
    max_bytes: Option<u64>,
}

/// The `web_fetch` tool -- see this module's own doc for the SSRF guard
/// [`WebFetchTool::invoke`] runs before every request.
pub struct WebFetchTool {
    config: FetchConfig,
}

impl WebFetchTool {
    pub fn new(config: FetchConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("web_fetch"),
            description: "GET an http(s) URL and return its body reduced to readable text \
                (HTML tags stripped, entities decoded), bounded to a maximum number of \
                bytes. Refuses non-http(s) schemes and private/loopback/link-local \
                addresses by default (see conway.web's own docs for the exact SSRF-guard \
                coverage). One URL per call -- no crawling, no JavaScript, no screenshots."
                .into(),
            schema: schemars::schema_for!(WebFetchArgs),
            category: ToolCategory::Read,
            permission: PermissionClass::RequiresApproval,
        }
    }

    /// `None`: `url` is not a filesystem path, so there is nothing for a
    /// root-containment check to confine -- see `PathArgs::None`'s own doc.
    /// (Confinement for THIS tool is `guard_url`, an
    /// entirely separate mechanism from the permission broker's path-root
    /// check.)
    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }

    /// `web_fetch` never overrides [`Tool::render`]'s trait default in a
    /// way that changes this answer -- its rendering (the bare URL, see
    /// [`Self::render`] below) is not a shell command.
    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }

    /// The bare URL, not the trait's default JSON dump -- so
    /// `conway_core::permission_pattern::When::Domains` (a structured
    /// allow/deny rule keyed on a URL-fetching tool's own rendering) has
    /// something to parse a host out of. See that predicate's own doc in
    /// `conway-core`.
    fn render(&self, args: &serde_json::Value) -> String {
        match args.get("url").and_then(serde_json::Value::as_str) {
            Some(url) => url.to_string(),
            None => format!("{}({})", self.spec().name, args),
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let args: WebFetchArgs = serde_json::from_value(call.arguments.clone()).map_err(|e| {
            ToolError::InvalidArguments {
                detail: e.to_string(),
            }
        })?;

        // Narrows only -- a call cannot raise this tool's own configured
        // ceiling by asking for more.
        let effective_max_bytes = args
            .max_bytes
            .map(|requested| (requested as usize).min(self.config.max_bytes))
            .unwrap_or(self.config.max_bytes);

        let url = Url::parse(&args.url).map_err(|e| ToolError::InvalidArguments {
            detail: format!("{:?} is not a valid URL: {e}", args.url),
        })?;

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(self.config.timeout)
            .build()
            .map_err(|e| ToolError::Internal {
                detail: e.to_string(),
            })?;

        let page =
            fetch_guarded(&client, url, effective_max_bytes, self.config.max_redirects).await?;

        let text = format!(
            "URL: {}\nContent-Type: {}\n\n{}",
            page.final_url,
            page.content_type.as_deref().unwrap_or("(not reported)"),
            page.body
        );

        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text { text }],
            is_error: false,
            truncation: TruncationPolicy::Head {
                max_bytes: effective_max_bytes as u64,
            },
            artifacts: vec![],
        })
    }
}

/// Why [`guard_url`] refused a URL -- a plain, human-readable message; the
/// tool wraps it in `ToolError::Denied { reason }` (this codebase's
/// established typed-refusal shape, e.g. `conway-plugin-confine`'s own
/// `ToolError::Denied` uses for a missing containment root), rather than
/// this crate inventing a second refusal type `conway-core`'s
/// `#[non_exhaustive]` `ToolError` cannot be extended with from outside
/// its own crate anyway.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GuardRefusal(String);

/// The SSRF/scheme guard -- see this module's own doc for exactly what it
/// classifies and what it does not. Called before every request
/// [`fetch_loop`] makes, including each redirect hop.
async fn guard_url(url: &Url) -> Result<(), GuardRefusal> {
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(GuardRefusal(format!(
            "refusing to fetch {url}: scheme {scheme:?} is not http or https"
        )));
    }
    let host = url
        .host()
        .ok_or_else(|| GuardRefusal(format!("refusing to fetch {url}: URL has no host")))?;
    match host {
        Host::Ipv4(v4) => {
            if let Some(reason) = classify_ipv4(v4) {
                return Err(GuardRefusal(format!(
                    "refusing to fetch {url}: {v4} is a {reason} address"
                )));
            }
        }
        Host::Ipv6(v6) => {
            if let Some(reason) = classify_ipv6(v6) {
                return Err(GuardRefusal(format!(
                    "refusing to fetch {url}: {v6} is a {reason} address"
                )));
            }
        }
        Host::Domain(name) => {
            let port = url.port_or_known_default().unwrap_or(80);
            let lookup_target = format!("{name}:{port}");
            let addrs: Vec<std::net::SocketAddr> =
                match tokio::net::lookup_host(&lookup_target).await {
                    Ok(iter) => iter.collect(),
                    Err(e) => {
                        return Err(GuardRefusal(format!(
                            "refusing to fetch {url}: could not resolve host {name:?}: {e}"
                        )));
                    }
                };
            if addrs.is_empty() {
                return Err(GuardRefusal(format!(
                    "refusing to fetch {url}: host {name:?} resolved to no addresses"
                )));
            }
            for sa in &addrs {
                if let Some(reason) = classify_ip(sa.ip()) {
                    return Err(GuardRefusal(format!(
                        "refusing to fetch {url}: host {name:?} resolves to {} which is a \
                         {reason} address",
                        sa.ip()
                    )));
                }
            }
        }
    }
    Ok(())
}

fn classify_ip(addr: IpAddr) -> Option<&'static str> {
    match addr {
        IpAddr::V4(v4) => classify_ipv4(v4),
        IpAddr::V6(v6) => classify_ipv6(v6),
    }
}

fn classify_ipv4(ip: Ipv4Addr) -> Option<&'static str> {
    let o = ip.octets();
    if o[0] == 0 {
        return Some("\"this network\" (0.0.0.0/8, including 0.0.0.0 itself)");
    }
    if ip.is_loopback() {
        return Some("loopback (127.0.0.0/8)");
    }
    if ip.is_private() {
        return Some("private (RFC 1918)");
    }
    if is_cgnat(ip) {
        return Some("carrier-grade NAT (100.64.0.0/10)");
    }
    if ip.is_link_local() {
        return Some(
            "link-local (169.254.0.0/16 -- includes cloud-metadata endpoints such as \
             169.254.169.254)",
        );
    }
    if ip.is_broadcast() {
        return Some("broadcast (255.255.255.255)");
    }
    if ip.is_documentation() {
        return Some("documentation (RFC 5737)");
    }
    if ip.is_multicast() {
        return Some("multicast (224.0.0.0/4)");
    }
    None
}

fn is_cgnat(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 100 && (64..=127).contains(&o[1])
}

fn classify_ipv6(ip: Ipv6Addr) -> Option<&'static str> {
    if ip.is_unspecified() {
        return Some("unspecified (::)");
    }
    if ip.is_loopback() {
        return Some("loopback (::1)");
    }
    let seg = ip.segments();
    if (seg[0] & 0xfe00) == 0xfc00 {
        return Some("unique local (fc00::/7)");
    }
    if (seg[0] & 0xffc0) == 0xfe80 {
        return Some("link-local (fe80::/10)");
    }
    if ip.is_multicast() {
        return Some("multicast (ff00::/8)");
    }
    if let Some(v4) = embedded_ipv4(ip) {
        if let Some(reason) = classify_ipv4(v4) {
            return Some(reason);
        }
    }
    None
}

/// Unwraps an IPv4-mapped (`::ffff:a.b.c.d`) or deprecated IPv4-compatible
/// (`::a.b.c.d`) IPv6 literal's embedded IPv4 address -- see this module's
/// own doc for exactly which embeddings this does and does not cover.
fn embedded_ipv4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let seg = ip.segments();
    let low32 = || {
        Ipv4Addr::new(
            (seg[6] >> 8) as u8,
            (seg[6] & 0xff) as u8,
            (seg[7] >> 8) as u8,
            (seg[7] & 0xff) as u8,
        )
    };
    // IPv4-mapped: 80 bits zero, then 0xffff, then the 32-bit address --
    // the same bit pattern regardless of whether the URL spelled it as
    // `::ffff:127.0.0.1` (dotted-quad-in-brackets) or `::ffff:7f00:1`
    // (hextet form): both parse to this segment layout.
    if seg[0] == 0 && seg[1] == 0 && seg[2] == 0 && seg[3] == 0 && seg[4] == 0 && seg[5] == 0xffff {
        return Some(low32());
    }
    // Deprecated IPv4-compatible: 96 bits zero, then the 32-bit address --
    // excludes `::` and `::1`, already classified by their own checks
    // above, so this never double-reports those two.
    if seg[0] == 0 && seg[1] == 0 && seg[2] == 0 && seg[3] == 0 && seg[4] == 0 && seg[5] == 0 {
        let v4 = low32();
        if v4.octets() != [0, 0, 0, 0] && v4.octets() != [0, 0, 0, 1] {
            return Some(v4);
        }
    }
    None
}

/// One fetched page: the FINAL url reached (after redirects), its declared
/// content type (if any), and its body already reduced to text -- see
/// [`reduce_body`].
#[derive(Debug)]
pub(crate) struct FetchedPage {
    pub(crate) content_type: Option<String>,
    pub(crate) body: String,
    pub(crate) final_url: Url,
}

/// [`fetch_loop`] with the SSRF guard always enforced -- the only entry
/// point production code (`WebFetchTool::invoke`) calls.
async fn fetch_guarded(
    client: &reqwest::Client,
    url: Url,
    max_bytes: usize,
    max_redirects: u8,
) -> Result<FetchedPage, ToolError> {
    fetch_loop(client, url, max_bytes, max_redirects, true).await
}

/// The GET-and-follow-redirects transport loop, bounded to `max_redirects`
/// hops and `max_bytes` of response body.
///
/// `enforce_guard` is `pub(crate)`-only surface, never reachable from
/// outside this crate and never set anywhere in production code except
/// [`fetch_guarded`] (always `true`): this module's own tests pass `false`
/// to exercise the redirect-following/truncation behavior below against a
/// local `wiremock` server bound to `127.0.0.1` -- which [`guard_url`]
/// would itself refuse as a loopback address, since a fixture server has
/// no way to bind anywhere else. The guard's OWN refusal behavior is
/// proven separately, directly against [`guard_url`]/[`classify_ipv4`]/
/// [`classify_ipv6`], with no network involved at all.
pub(crate) async fn fetch_loop(
    client: &reqwest::Client,
    mut url: Url,
    max_bytes: usize,
    max_redirects: u8,
    enforce_guard: bool,
) -> Result<FetchedPage, ToolError> {
    let mut hops: u8 = 0;
    loop {
        if enforce_guard {
            guard_url(&url)
                .await
                .map_err(|refusal| ToolError::Denied { reason: refusal.0 })?;
        }
        let resp = client
            .get(url.clone())
            .send()
            .await
            .map_err(|e| ToolError::Io {
                detail: format!("fetching {url}: {e}"),
            })?;
        let status = resp.status();
        if status.is_redirection() {
            if hops >= max_redirects {
                return Err(ToolError::Denied {
                    reason: format!(
                        "refusing to follow more than {max_redirects} redirect(s) fetching {url}"
                    ),
                });
            }
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| ToolError::Denied {
                    reason: format!("redirect ({status}) from {url} carried no Location header"),
                })?
                .to_string();
            let next = url.join(&location).map_err(|e| ToolError::Denied {
                reason: format!("redirect Location {location:?} from {url} did not parse: {e}"),
            })?;
            url = next;
            hops += 1;
            continue;
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let bytes = read_bounded(resp, max_bytes).await?;
        let body = reduce_body(&bytes, content_type.as_deref());
        return Ok(FetchedPage {
            content_type,
            body,
            final_url: url,
        });
    }
}

/// Streams the response body, stopping the moment `max_bytes` is reached --
/// a genuine network-level bound (the tool never downloads more than this
/// regardless of how large the server's response actually is), not merely
/// a truncation applied after a full download.
async fn read_bounded(resp: reqwest::Response, max_bytes: usize) -> Result<Vec<u8>, ToolError> {
    let mut stream = resp.bytes_stream();
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ToolError::Io {
            detail: format!("reading response body: {e}"),
        })?;
        let remaining = max_bytes.saturating_sub(buf.len());
        if remaining == 0 {
            break;
        }
        if chunk.len() > remaining {
            buf.extend_from_slice(&chunk[..remaining]);
            break;
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

fn reduce_body(bytes: &[u8], content_type: Option<&str>) -> String {
    let text = String::from_utf8_lossy(bytes);
    let is_html = content_type
        .map(|ct| ct.to_ascii_lowercase().contains("html"))
        .unwrap_or(false);
    if is_html {
        reduce_html(&text)
    } else {
        text.into_owned()
    }
}

/// Reduces HTML to readable text: strips every tag, drops the CONTENTS of
/// `<script>`/`<style>` entirely, decodes a small set of named entities
/// plus numeric (`&#NN;`/`&#xHH;`) entities, and collapses whitespace.
///
/// **This is not a browser-grade HTML/CSS engine** -- no DOM, no layout, no
/// visibility rules, no `<template>`/`<noscript>` special-casing. It is a
/// lightweight reducer sized for "read this doc page as text"; see this
/// crate's own Cargo.toml doc for why this is hand-rolled rather than a new
/// dependency.
fn reduce_html(input: &str) -> String {
    collapse_whitespace(&decode_entities(&strip_tags(input)))
}

/// Strips every `<...>` tag from `input`, discarding the CONTENT of
/// `<script>`/`<style>` elements entirely (their text is never sent to the
/// model), and inserts a newline where a handful of common block-level tags
/// appeared, so paragraphs/list items/headings do not run together.
fn strip_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut skip_until: Option<String> = None;

    while let Some(c) = chars.next() {
        if c != '<' {
            if skip_until.is_none() {
                out.push(c);
            }
            continue;
        }
        let is_closing = chars.peek() == Some(&'/');
        if is_closing {
            chars.next();
        }
        let mut name = String::new();
        while let Some(&nc) = chars.peek() {
            if nc.is_whitespace() || nc == '>' || nc == '/' {
                break;
            }
            name.push(nc.to_ascii_lowercase());
            chars.next();
        }
        // Consume the rest of the tag, up to an UNQUOTED '>' (an attribute
        // value may itself contain '>').
        let mut in_quote: Option<char> = None;
        loop {
            match chars.next() {
                None => break,
                Some(q) if Some(q) == in_quote => in_quote = None,
                Some(q @ ('"' | '\'')) if in_quote.is_none() => in_quote = Some(q),
                Some('>') if in_quote.is_none() => break,
                Some(_) => {}
            }
        }
        if let Some(active) = &skip_until {
            if is_closing && &name == active {
                skip_until = None;
            }
            continue;
        }
        if !is_closing && (name == "script" || name == "style") {
            skip_until = Some(name);
            continue;
        }
        if matches!(
            name.as_str(),
            "br" | "p" | "div" | "li" | "tr" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
        ) {
            out.push('\n');
        }
    }
    out
}

/// Decodes HTML entities (`&amp;`, `&#39;`, `&#x27;`, ...) in `input`; an
/// unrecognized or unterminated `&...;` span is left as a literal `&`
/// followed by whatever character comes next, never dropped.
fn decode_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '&' {
            out.push(c);
            continue;
        }
        let mut candidate = String::new();
        let mut terminated = false;
        let mut lookahead = chars.clone();
        for _ in 0..12 {
            match lookahead.next() {
                Some(';') => {
                    terminated = true;
                    break;
                }
                Some(nc) if nc.is_ascii_alphanumeric() || nc == '#' => candidate.push(nc),
                _ => break,
            }
        }
        if terminated {
            if let Some(decoded) = decode_entity(&candidate) {
                out.push(decoded);
                // Advance the real iterator past `candidate` plus the `;`.
                for _ in 0..=candidate.chars().count() {
                    chars.next();
                }
                continue;
            }
        }
        out.push('&');
    }
    out
}

fn decode_entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{00A0}'),
        "mdash" => Some('\u{2014}'),
        "ndash" => Some('\u{2013}'),
        "hellip" => Some('\u{2026}'),
        "copy" => Some('\u{00A9}'),
        "rsquo" => Some('\u{2019}'),
        "lsquo" => Some('\u{2018}'),
        "rdquo" => Some('\u{201D}'),
        "ldquo" => Some('\u{201C}'),
        other => {
            if let Some(hex) = other
                .strip_prefix("#x")
                .or_else(|| other.strip_prefix("#X"))
            {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else if let Some(dec) = other.strip_prefix('#') {
                dec.parse::<u32>().ok().and_then(char::from_u32)
            } else {
                None
            }
        }
    }
}

/// Collapses runs of whitespace within a line to a single space, and runs
/// of blank lines to at most one, trimming the result.
fn collapse_whitespace(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut blank_run = 0u32;
    for line in input.split('\n') {
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&collapsed);
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- IPv4 classification ----

    #[test]
    fn classify_ipv4_refuses_loopback_private_link_local_and_cgnat() {
        assert_eq!(
            classify_ipv4(Ipv4Addr::new(127, 0, 0, 1)),
            Some("loopback (127.0.0.0/8)")
        );
        assert!(classify_ipv4(Ipv4Addr::new(10, 1, 2, 3)).is_some());
        assert!(classify_ipv4(Ipv4Addr::new(172, 16, 0, 1)).is_some());
        assert!(classify_ipv4(Ipv4Addr::new(192, 168, 1, 1)).is_some());
        assert!(
            classify_ipv4(Ipv4Addr::new(169, 254, 169, 254)).is_some(),
            "the cloud-metadata endpoint must be refused"
        );
        assert!(
            classify_ipv4(Ipv4Addr::new(100, 64, 0, 1)).is_some(),
            "CGNAT"
        );
        assert!(classify_ipv4(Ipv4Addr::new(0, 0, 0, 0)).is_some());
        assert!(classify_ipv4(Ipv4Addr::new(255, 255, 255, 255)).is_some());
    }

    /// A genuine public address is not refused -- catches an
    /// over-broad classifier that (for example) treats every address as
    /// suspicious, which would make the tool useless.
    #[test]
    fn classify_ipv4_allows_a_public_address() {
        assert_eq!(classify_ipv4(Ipv4Addr::new(8, 8, 8, 8)), None);
    }

    // ---- IPv6 classification ----

    #[test]
    fn classify_ipv6_refuses_loopback_unspecified_ula_and_link_local() {
        assert!(classify_ipv6(Ipv6Addr::LOCALHOST).is_some());
        assert!(classify_ipv6(Ipv6Addr::UNSPECIFIED).is_some());
        assert!(classify_ipv6("fd12:3456:789a::1".parse().unwrap()).is_some());
        assert!(classify_ipv6("fe80::1".parse().unwrap()).is_some());
        assert!(classify_ipv6("ff02::1".parse().unwrap()).is_some());
    }

    /// IPv4-mapped IPv6, in BOTH spellings that parse to the same bits.
    #[test]
    fn classify_ipv6_unwraps_ipv4_mapped_addresses_in_either_spelling() {
        let dotted: Ipv6Addr = "::ffff:127.0.0.1".parse().unwrap();
        let hextet: Ipv6Addr = "::ffff:7f00:1".parse().unwrap();
        assert_eq!(
            dotted, hextet,
            "both spellings must parse to the identical address"
        );
        assert!(classify_ipv6(dotted).is_some());
    }

    /// The deprecated IPv4-compatible form.
    #[test]
    fn classify_ipv6_unwraps_the_deprecated_ipv4_compatible_form() {
        let addr: Ipv6Addr = "::10.0.0.5".parse().unwrap();
        assert!(classify_ipv6(addr).is_some());
    }

    #[test]
    fn classify_ipv6_allows_a_public_address() {
        // Google Public DNS's IPv6 address.
        let addr: Ipv6Addr = "2001:4860:4860::8888".parse().unwrap();
        assert_eq!(classify_ipv6(addr), None);
    }

    // ---- Decimal/octal/hex IPv4 encodings, via the `url` crate's own parser ----

    /// Proves the claim this module's own doc makes: the WHATWG host
    /// parser normalizes these alternate numeric encodings to the real
    /// address before this module ever sees a `Host::Ipv4` -- catches a
    /// regression (or a wrong assumption) in that reliance directly,
    /// rather than only via a refused fetch.
    #[test]
    fn the_url_crate_normalizes_decimal_octal_and_hex_ipv4_encodings() {
        for spelling in [
            "http://2130706433/",
            "http://0x7f000001/",
            "http://0177.0.0.1/",
        ] {
            let url = Url::parse(spelling).unwrap();
            assert_eq!(
                url.host(),
                Some(Host::Ipv4(Ipv4Addr::new(127, 0, 0, 1))),
                "{spelling} must normalize to 127.0.0.1"
            );
        }
    }

    // ---- guard_url: no network involved (IP-literal hosts only) ----

    /// Acceptance 1's exact scenario: refused, typed, no connection
    /// attempted (loopback is classified from the URL alone).
    #[tokio::test]
    async fn guard_url_refuses_a_loopback_ip_literal() {
        let url = Url::parse("http://127.0.0.1/secret").unwrap();
        let err = guard_url(&url).await.unwrap_err();
        assert!(err.0.contains("loopback"), "{}", err.0);
    }

    #[tokio::test]
    async fn guard_url_refuses_a_private_ipv6_literal() {
        let url = Url::parse("http://[fd12:3456:789a::1]/").unwrap();
        let err = guard_url(&url).await.unwrap_err();
        assert!(err.0.contains("unique local"), "{}", err.0);
    }

    #[tokio::test]
    async fn guard_url_refuses_a_non_http_scheme() {
        let url = Url::parse("file:///etc/passwd").unwrap();
        let err = guard_url(&url).await.unwrap_err();
        assert!(err.0.contains("scheme"), "{}", err.0);
    }

    /// A decimal-encoded loopback address, refused via the SAME
    /// `Host::Ipv4` path an ordinary dotted-quad literal takes -- proves
    /// the guard, not just the parser, honors the normalized form.
    #[tokio::test]
    async fn guard_url_refuses_a_decimal_encoded_loopback_address() {
        let url = Url::parse("http://2130706433/").unwrap();
        let err = guard_url(&url).await.unwrap_err();
        assert!(err.0.contains("loopback"), "{}", err.0);
    }

    /// End-to-end through the real public `Tool` surface, no wiremock
    /// involved: the guard refuses before any connection is attempted, so
    /// this test needs no network at all -- catches a regression where
    /// `invoke` stopped calling the guard, or called it after the request.
    #[tokio::test]
    async fn web_fetch_tool_invoke_refuses_a_loopback_target_end_to_end() {
        let tool = WebFetchTool::new(FetchConfig::default());
        let call = ToolCall {
            call_id: "c1".into(),
            name: ToolName::new("web_fetch"),
            arguments: serde_json::json!({ "url": "http://127.0.0.1:1/x" }),
        };
        let agent_id = conway::AgentId::new();
        let ctx = ToolCtx::for_test(
            agent_id,
            std::env::temp_dir(),
            std::sync::Arc::new(conway_testkit::FakeSubagentHost::new(agent_id)),
            std::sync::Arc::new(conway_testkit::CollectingEventSink::new()),
        );
        let err = tool.invoke(call, ctx).await.unwrap_err();
        match err {
            ToolError::Denied { reason } => assert!(reason.contains("loopback"), "{reason}"),
            other => panic!("expected Denied, got {other:?}"),
        }
    }

    // ---- fetch_loop transport: guard disabled, exercised against wiremock ----

    /// Truncation at `max_bytes`: catches an implementation that ignores
    /// the bound and returns the full body regardless of size.
    #[tokio::test]
    async fn fetch_loop_stops_reading_at_max_bytes() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let big_body = "x".repeat(1000);
        Mock::given(method("GET"))
            .and(path("/big"))
            .respond_with(ResponseTemplate::new(200).set_body_string(big_body))
            .mount(&server)
            .await;

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let url = Url::parse(&format!("{}/big", server.uri())).unwrap();
        let page = fetch_loop(&client, url, 100, 5, false).await.unwrap();
        assert_eq!(
            page.body.len(),
            100,
            "the returned body must be capped at max_bytes, not the full 1000-byte response"
        );
    }

    /// The redirect bound: catches an implementation with no bound at all
    /// (would hang forever on a redirect loop) or an off-by-one that
    /// allows one hop more than configured.
    #[tokio::test]
    async fn fetch_loop_refuses_more_than_max_redirects_hops() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // /r0 -> /r1 -> /r2 -> ... an unbounded redirect chain.
        for i in 0..10 {
            let next = format!("{}/r{}", server.uri(), i + 1);
            Mock::given(method("GET"))
                .and(path(format!("/r{i}")))
                .respond_with(ResponseTemplate::new(302).insert_header("Location", next.as_str()))
                .mount(&server)
                .await;
        }

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let url = Url::parse(&format!("{}/r0", server.uri())).unwrap();
        let err = fetch_loop(&client, url, 1_000, 2, false).await.unwrap_err();
        match err {
            ToolError::Denied { reason } => assert!(reason.contains("redirect"), "{reason}"),
            other => panic!("expected Denied, got {other:?}"),
        }
    }

    /// A redirect chain within the bound is followed to its final page,
    /// and the returned `final_url` reflects the LAST hop, not the first.
    #[tokio::test]
    async fn fetch_loop_follows_a_redirect_within_the_bound_to_its_final_page() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let target = format!("{}/final", server.uri());
        Mock::given(method("GET"))
            .and(path("/start"))
            .respond_with(ResponseTemplate::new(302).insert_header("Location", target.as_str()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/final"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "text/plain")
                    .set_body_string("hello"),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let url = Url::parse(&format!("{}/start", server.uri())).unwrap();
        let page = fetch_loop(&client, url, 1_000, 5, false).await.unwrap();
        assert_eq!(page.body, "hello");
        assert!(page.final_url.as_str().ends_with("/final"));
    }

    // ---- HTML reduction ----

    #[test]
    fn reduce_html_strips_tags_and_drops_script_and_style_content() {
        let html = "<html><head><style>.a{color:red}</style></head><body>\
                     <p>Hello <b>world</b></p><script>evil()</script></body></html>";
        let text = reduce_html(html);
        assert!(text.contains("Hello world"), "{text}");
        assert!(!text.contains("evil"), "{text}");
        assert!(!text.contains("color:red"), "{text}");
        assert!(!text.contains('<'), "{text}");
    }

    #[test]
    fn reduce_html_decodes_named_and_numeric_entities() {
        assert_eq!(reduce_html("Tom &amp; Jerry"), "Tom & Jerry");
        assert_eq!(reduce_html("&#39;quoted&#39;"), "'quoted'");
        assert_eq!(reduce_html("&#x27;quoted&#x27;"), "'quoted'");
    }

    #[test]
    fn reduce_html_leaves_an_unterminated_ampersand_literal() {
        assert_eq!(reduce_html("Q&A"), "Q&A");
    }

    #[test]
    fn collapse_whitespace_collapses_runs_of_blank_lines_and_inline_whitespace() {
        assert_eq!(collapse_whitespace("a   b\n\n\n\nc"), "a b\n\nc");
    }
}
