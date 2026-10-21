# Changelog

All notable changes are listed here. The project follows [Semantic Versioning](https://semver.org/).

## [0.12.1] — 2026-10-22

### Fixed
- macOS: low-memory mode now reads the available memory (via `vm_stat`) and serialises browser work as on Linux; it was ignored before.
- A click or other action that navigates the page no longer makes the next script call fail with "Inspected target navigated or closed".
- Release builds for Intel Macs run on `macos-15-intel` (GitHub retired `macos-13`).

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
