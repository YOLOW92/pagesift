# Changelog

All notable changes are listed here. The project follows [Semantic Versioning](https://semver.org/).

## [0.12.0] — unreleased

The first public release.

### License
- pagesift is now dual-licensed: **AGPL-3.0-only** for open-source use, and a **commercial license** for use without the AGPL's obligations (see `COMMERCIAL.md`). Contributions are accepted under a CLA. Earlier development snapshots were published under MIT OR Apache-2.0 and remain available under those terms.

### Scraping and extraction
- In-house HTML5 parser and main-content extractor: 0.47 ms per page (p50, single thread) and the best score on Firecrawl's labelled dataset among the tools tested (see `docs/benchmarks.md`).
- Formats: `markdown`, `html`, `rawHtml`, `links`, `images`; `summary` and `json` with an OpenAI-compatible LLM; `changeTracking` with the cache.
- `queryFilter` keeps only the passages relevant to a query (BM25).
- Metadata: title, description, language, author, published and modified time, Open Graph and similar tags; PDF text extraction.
- Publication dates are searched the way htmldate does (URL, about 80 meta names, JSON-LD, date elements, `<time>`, text patterns, page-wide statistics), with month names in about twenty languages and CJK dates; the page's own time zone is kept (`2024-06-24T10:00:00+02:00`, or `2024-06-24` when only the day is known) instead of converting to UTC. On htmldate's annotated set: F1 0.948, the same as htmldate, at 0.06 ms per page.
- Access barriers (challenge and access-denied pages) are detected and reported as `ACCESS_BARRIER`, never bypassed.

### Crawling
- Firecrawl v2-compatible crawl, map, batch scrape, search and extract endpoints.
- Query-focused and adaptive crawling (`query`, `adaptive`): most relevant pages first, stop when new pages stop helping.
- In-house HTTP client (HTTP/1.1 and HTTP/2, gzip/deflate/brotli/zstd), per-host rate limiting with backoff, robots.txt with `Crawl-delay`, bounded frontier and memory.
- Crawl frontier built for real sites: tracking parameters and session IDs are stripped before fetching; images, media, archives and fonts are skipped; self-repeating paths are rejected; URL rewrite rules (`index.html`, `?print=1`, …) are learned from duplicate pages; URL templates that keep returning duplicate content (calendars, endless pagination, sort/filter combinations) are moved behind normal pages; exact duplicates are marked with `metadata.pagesift.duplicateOf`.
- Transient failures (timeouts, network errors, 5xx, 429) are retried later without blocking the rest of the crawl; a host that returns 429 gets a wider request interval that recovers gradually.
- Crawls that span subdomains or external hosts queue URLs per host and take hosts in turn, so a long queue on one host never blocks the others.
- Sitemaps: XML, RSS/Atom feeds and plain-text sitemaps; common sitemap locations are tried when robots.txt names none.

### Rendering
- Own CDP client and browser pool; automatic engine choice and fallback: Lightpanda → moli → Chrome/Chromium/chrome-headless-shell, or a remote browser.
- The pool sizes itself from measured throughput, CPU and free memory; idle browsers are closed.
- Per-site learning: skips rendering on sites where it adds nothing, and starts from the engine that works for the site.
- Same-origin iframes are inlined (over HTTP when the engine can't render them); ads, trackers, fonts and media are blocked by default.

### Service
- HTTP API with API keys, per-key rate, concurrency and daily page limits; job persistence and resume; signed webhooks with retries; Prometheus metrics; JSON logs; graceful shutdown.
- Total per-request deadlines (`timeout`), named egress proxies (`egress`), allowlisted internal hosts, and distinct error codes: `DNS_FAILURE`, `NETWORK_ERROR`, `BUSY`, `TIMEOUT`, `SSRF_BLOCKED`, `ACCESS_BARRIER`, `RATE_LIMITED`, …
- SSRF protection on requests, redirects, browser sub-requests and webhooks, with DNS-rebinding checks.
- Docker image with Lightpanda; MCP server; CLI (`scrape`, `crawl`, `map`, `serve`, `mcp`, `convert`, `doctor`, `bench`).

### Bindings
- Python package (`pip install pagesift`) and Node.js package (`npm install pagesift`) embedding the engine.

[0.12.0]: https://github.com/YOLOW92/pagesift/releases/tag/v0.12.0
