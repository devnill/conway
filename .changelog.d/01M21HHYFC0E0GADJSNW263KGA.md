### Fixed

- **`web_fetch` now connects to the exact address its own SSRF guard vetted, closing a DNS-rebinding window** — board item `01M21HHYFC0E0GADJSNW263KGA`. The guard previously resolved a domain name once, classified that answer, and then handed the bare hostname to `reqwest`, which resolved it AGAIN to actually connect — a hostile DNS server could answer a public address the first time and a private one the second, defeating the guard. `guard_url` (`crates/conway-plugin-web/src/fetch.rs`) now returns the exact address(es) it classified, and every hop (the initial request and each re-guarded redirect) installs a custom `reqwest::dns::Resolve` pinned to that vetted set on a client built fresh for that hop — the connection provably goes to the classified address, or the call is refused; it never falls back to an unpinned connect. This pins the connection only, not the URL: the `Host` header and TLS SNI still name the original hostname, so certificate verification and virtual hosting are unaffected. The tunneled-address (6to4/Teredo/NAT64) disclosure in `docs/plugins/web.md` is unchanged — this item does not touch that gap.

### Changed

- **`docs/plugins/web.md`** removes the DNS-rebinding entry from `web_fetch`'s "what this does NOT cover" list and states the pinning fix in its place.
