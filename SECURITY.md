# Security policy

pagesift fetches URLs on behalf of its callers, so it is a natural target for server-side request forgery (SSRF). We treat these as security bugs:

- reaching private, loopback, link-local or cloud-metadata addresses when `allow_private_network` is off — through redirects, DNS rebinding, IP literal tricks, browser sub-requests, iframes, or webhooks;
- bypassing robots.txt handling or the per-host rate limits;
- leaking API keys, credentials configured for one host to another host, or job data between API keys;
- crashes or unbounded memory use triggered by a page or a request (denial of service).

## Supported versions

| Version | Supported |
|---|---|
| 2.0.x | ✅ |
| < 2.0 | ❌ |

## Reporting a vulnerability

Please report privately through GitHub's **Report a vulnerability** button on the repository's Security tab (private vulnerability reporting). Include the pagesift version, the configuration, and a reproduction if you can.

We aim to acknowledge reports within 3 working days and to ship a fix for confirmed issues within 30 days, and we will credit you in the release notes unless you prefer otherwise. Please give us a chance to fix the issue before disclosing it publicly.
