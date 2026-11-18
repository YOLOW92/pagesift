# Security policy

pagesift fetches untrusted URLs, executes optional headless browser actions, parses untrusted binary documents, stores crawled graphs/archives, and serves multi-tenant API, MCP, and cluster endpoints. We treat vulnerabilities across these trust boundaries as security bugs:

## In scope

- **SSRF & network isolation.** Reaching private, loopback, link-local, or cloud-metadata addresses when `allow_private_network` is off — via redirects, DNS rebinding, IP literal tricks, browser sub-requests, iframes, or webhooks.
- **Politeness & egress policy bypass.** Bypassing `robots.txt` rules or per-site rate limits, including by splitting traffic across egress IPs or rotating egress after `429` / `503 Retry-After` responses.
- **Auth, credentials & multi-tenant isolation.** Bypassing API/MCP/cluster authentication, leaking host-scoped credentials/cookies to other hosts, leaking job data between API keys, or leaking private target context (`headers`, `keepSession`, `actions`) into anonymous or cross-tenant DataGraph / Sync scopes.
- **Browser sandbox & host execution.** Escaping configured browser/CDP boundaries via `actions`, `file://` URLs, or local resource access (pagesift refuses to disable the Chrome sandbox or run local Chromium as `root`).
- **Untrusted input DoS, path traversal & memory safety.** Crashes, unbounded memory/disk growth (e.g., zip/decompression bombs, malformed HTML/PDF/Office/archive inputs), path traversal or symlink escapes in workspace store/WARC/snapshot writes, or memory-safety defects in `unsafe` FFI, allocator, GPU, or SIMD inference paths (`pagesift-sys`, `pagesift-ffi`, `pagesift-gpu`, `pagesift-infer`).

## Out of scope

- Deployments where the operator explicitly enables `allow_private_network = true`, enables `browserFingerprint`, or exposes an unauthenticated listener on a public interface.
- Target sites returning `403`, CAPTCHAs, or rate limits against compliant crawling without an engine policy bypass.

## Supported versions

Security fixes go into the latest release line only; upgrade to its newest patch release.

| Version | Supported |
|---|---|
| 1.6.x | ✅ |
| < 1.6 | ❌ |

## Reporting a vulnerability

Please report privately through GitHub's **Report a vulnerability** button on the repository's **Security** tab (Private Vulnerability Reporting). If you cannot use GitHub Private Vulnerability Reporting, email **[yolow92@outlook.com](mailto:yolow92@outlook.com)** with `[SECURITY]` in the subject line.

Include the `pagesift` version, relevant configuration, deployment mode (CLI, server, MCP, Python/Node, cluster), and a minimal reproduction if possible.

We aim to acknowledge reports within 3 working days and to ship a fix for confirmed issues within 30 days, and we will credit you in the release notes unless you prefer otherwise. Please give us a chance to fix the issue before disclosing it publicly.
