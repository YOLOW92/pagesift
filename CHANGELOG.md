# Changelog

All notable changes are listed here. The project follows [Semantic Versioning](https://semver.org/).

## [0.30.0] — 2026-10-23

Modular architecture refactoring, script navigation folding, metadata and publication date acceleration, RAG chunking and QueryFilter speedups, and comprehensive benchmark suite expansion under a strict 12 GiB memory ceiling. See [the release and migration guide](docs/releases/0.30.0.md).

### Added
- Bounded pure string-helper folding in `pagesift-content::script_links` for arrow and function declarations (`const fn = (a, b) => a + b`), raising controlled navigation discovery from 12/13 to 13/13 (100%) with zero false navigation visits.
- Dedicated `extract_metadata` fast path in `pagesift-content`, speeding up metadata-only queries to 0.32–0.37 ms/page (14× faster than `htmldate extensive`).
- Disambiguation rules for publication date parsing: prioritizing full three-component dates over two-component year-month anchors, slash timestamps, page title fallbacks, and percent-encoded digit filtering, achieving 0.950 F1 on the 1,000-page `htmldate` corpus.
- Binary-search token indexing over precomputed token starts for RAG chunking and relevance filtering, cutting BM25 QueryFilter processing time from 11.45 ms to 1.35–2.11 ms/question (788× faster than Crawl4AI BM25Filter).
- Comprehensive benchmark corpus expansion across 8,854 live random sites, 148 non-UTF-8 encodings, 120 wild-web sites, and 20,000 (`wide`) / 50,000 (`huge`) page HTTP crawls under a strict 12 GiB memory limit.

### Changed
- Refactored all 9 oversized source files (>1,000 lines) into clean, modular submodules (<600 lines each) across workspace crates with 100% backward compatibility.
- Workspace crates, npm packages, and Python bindings bumped to `0.30.0`.

## [0.20.0] — 2026-10-22

Native Rust collection and archiving cores, with bounded external-script discovery and an auditable six-project comparison. See [the release and migration guide](docs/releases/0.20.0.md). Browser interaction and autonomous Agent tasks are outside the 0.20.0 scope.

### Migration
- Rust workspace crates and all native package distributions move together to 0.20.0. Rust struct literals and exhaustive error matches may require changes; the HTTP API remains under `/v2`.
- Session snapshots, collection checkpoints and archive manifests are distinct from the URL cache. Back up state before upgrading; no automatic destructive migration is performed.
- New bounded cores are principally Rust APIs and CLI commands; Python/Node retain their existing scraping surface.

### Fixed
- Title-matched short prose can be isolated without article declarations. Compact editorial product callouts, procedural purchase links, video descriptions and substantial embedded quotations survive strict cleanup; explicitly attributed recommendation lists remain excluded.
- Article headline ownership prevents recommendation headings from expanding the story region; identified articles retain shorter trailing sections and separately placed document titles.
- Editorial quotation embeds survive social-wrapper cleanup. Undated article declarations can be corroborated by one substantial article region; CMS subheadings remain visible in strict article output.
- Thin pages containing client scripts or event handlers still render after neighboring static pages trained the auto-engine to skip unhelpful renders, including shared-path and query-routed applications.
- Persisted crawl and batch results carry a private completion receipt, allowing restart recovery to rebuild a missing done marker without replaying an already saved result. Batch identities remain positional.
- Learning that rendering adds no content is scoped to an origin and its first path segment, so short static pages do not suppress rendering in another section of the same site.
- Image metadata falls through placeholder or unsupported URLs to lazy-load attributes and supports lazy `srcset` attributes. Markdown image destinations reject unsupported schemes.

### Added
- Optional native Rust fuzzy candidates with abstention, form metadata/GET planning and redacted knowledge indicators; origin-scoped external module traversal and opt-in HTTP crawl integration.
- Explicit WordPress/Mastodon/Wikimedia/JSON-LD media adapters, recoverable named media/metadata publication, and bounded unencrypted HLS/DASH VOD planning and separate-track archiving. Protocol and ecosystem limits are documented in `docs/native-core.md`.
- Explicit native HTTP Cookie sessions with strict origin isolation, bounded storage, lifecycle/retirement accounting, private atomic snapshots and the `session-fetch` CLI; default requests remain stateless.
- Identity-addressed direct file archiving (`pagesift download`) with bounded raw streaming, strong-validator resume, prefix checksums, exclusive archive locking and recoverable atomic publication.
- Bounded static discovery of literal inline navigation and default GET calls; comments, data scripts, dynamic expressions and unknown/write options abstain.
- Business anchors allow element-tag changes while rejecting duplicate IDs across different tags. Incremental checkpoints store canonical BLAKE3 revisions instead of full item payloads and migrate earlier payload checkpoints on read.
- Serializable Rust business-identity anchors with exact/relocated/missing/ambiguous outcomes, plus semantic item discovery that rejects duplicate identities.
- Incremental `collect` CLI and Rust API with durable page checkpoints, acknowledged updates, opt-in terminal deletion sync, HTML identity discovery, explicit JSON mapping and a WordPress REST adapter. Delivery is at least once; a 32 MiB checkpoint limit bounds collection state.
- The Rust store exposes explicit source/item/revision completion records for incremental collectors, separate from URL caches and automatically evictable data. Callers commit after delivery; this does not automatically identify items in crawled pages.
- Document link discovery includes iframe/frame sources, HTMX GET attributes, and HTML pagination links. HTTP extraction also recognizes pagination Link response headers, preserving existing crawl scope and robots checks.

### Development
- Node asynchronous conversion reserves one logical CPU for the event loop and stages input copying before starting parser workers; the existing 150 ms responsiveness gate remains unchanged.
- Python release tests verify four platforms; Node release tests verify six targets, including native Windows x64 MSVC and Alpine x64 musl builds. Updated stale Node fingerprint and Python network-error assertions; concurrent conversion timing is recorded separately from the GIL-release correctness test.
- Added a resource-bounded, version-locked comparison harness and initial diagnostics for Firecrawl, Crawlee, Scrapling, Katana, Essence and gallery-dl; results distinguish component tests from complete services.
- The webhook delivery test waits for the sender to observe the HTTP acknowledgement before asserting its delivery metric.

## [0.12.1] — 2026-10-22

### Fixed
- macOS: low-memory mode now reads the available memory (via `vm_stat`) and serialises browser work as on Linux; it was ignored before.
- A click or other action that navigates the page no longer makes the next script call fail with "Inspected target navigated or closed".
- Release builds for Intel Macs run on `macos-15-intel` (GitHub retired `macos-13`), and the publish step no longer trips over Docker's build record. 0.12.1 is the first version on crates.io, PyPI and npm; 0.12.0 was only published as a Docker image.

## [0.12.0] — 2026-10-22

The first public release.

### License
- pagesift is now dual-licensed: **AGPL-3.0-only** for open-source use, and a **commercial license** for use without the AGPL's obligations (see `COMMERCIAL.md`). Contributions are accepted under a CLA. Earlier development snapshots were published under MIT OR Apache-2.0 and remain available under those terms.

### Scraping and extraction
- In-house HTML5 parser and main-content extractor: 0.47 ms per page (p50, single thread) and the best score on Firecrawl's labelled dataset among the tools tested (see `docs/benchmarks.md`).
- Formats: `markdown`, `html`, `rawHtml`, `links`, `images`; `summary` and `json` with an OpenAI-compatible LLM; `changeTracking` with the cache.
- Markdown fidelity: hard line breaks for `<br>`; formulas as TeX (`$…$`, `$$…$$`) from KaTeX, MathJax 2 and 3, Wikipedia and LaTeXML pages, with an own MathML→LaTeX converter and raw TeX left untouched; Markdown characters in text are escaped only where CommonMark would misread them. 1.000 on a random round-trip benchmark (next best 0.936).
- `queryFilter` keeps only what is relevant to a query: ~100-token units inside sections, BM25 with heading paths, units scoring at least half of the best (`relative`, default 0.5), optional `threshold`, `topK` and `maxTokens`; adjacent units are merged back into the original text. On a labelled RAG benchmark: 87% of the answer text in ~600 words (Crawl4AI's BM25 filter: 67% in 13,500 words), 70× faster.
- `chunking` (scrape option) returns the Markdown as chunks with heading paths, byte positions and token counts; strategies `recursive` (Rust port of semchunk with Markdown structure and CJK punctuation), `headings`, `paragraphs`, `sentences`, `fixed_tokens`, `sliding_window`, `overlapping`, `regex`.
- Metadata: title, description, language, author, published and modified time, Open Graph and similar tags.
- PDF to Markdown: tables become Markdown tables (ruled grids with pdfplumber's lattice algorithm, booktabs-style rules, framed boxes and borderless tables found from text alignment, with wrapped cells merged); inline formulas become `$…$` LaTeX with sub- and superscripts, accents and symbol commands, using the built-in encodings of embedded Type1 and CFF fonts and TeX glyph names; headers and footers isolated in a page margin are dropped. On olmOCR-bench's born-digital subsets: 71.0 overall, 44% of formula tests (other tools 0%), 56% of table tests, at 3.4 ms per page (pymupdf4llm: 64.4, 320 ms).
- Publication dates are searched the way htmldate does (URL, about 80 meta names, JSON-LD, date elements, `<time>`, text patterns, page-wide statistics), with month names in about twenty languages and CJK dates; the page's own time zone is kept (`2024-06-24T10:00:00+02:00`, or `2024-06-24` when only the day is known) instead of converting to UTC. On htmldate's annotated set: F1 0.948, the same as htmldate, at 0.06 ms per page.
- `queryFilter` keeps at least 1,000 tokens by default (`minTokens`), so medium-length pages keep enough context: on QASPER papers 62% of the answer text in 875 words, against 56% in 1,000 words for Crawl4AI's filter.
- Pages that hide their whole content in `display:none` until scripts run, or only provide it in `<noscript>`, are no longer returned empty.
- Release benchmark run with fresh data, the latest competitors and recorded seeds: `bench/fresh/results-2026-10-22.md`.
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

[0.20.0]: https://github.com/YOLOW92/pagesift/releases/tag/v0.20.0

[0.30.0]: https://github.com/YOLOW92/pagesift/releases/tag/v0.30.0
