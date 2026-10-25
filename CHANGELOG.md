# Changelog

All notable changes are listed here. The project follows [Semantic Versioning](https://semver.org/).

## [0.70.0] — 2026-10-26

NUMA-local data-plane rewrite, cgroup-aware capacity, and seven public extraction benchmarks.
See [the release notes](docs/releases/0.70.0.md).

### Added
- Hand-written NUMA-local HTTP/1.1 data plane: fixed-slot NUMA executors, minimal reactor,
  hybrid reactor/content scheduling, per-origin adaptive connections, sharded intake with
  bounded work stealing, and a fused parse pipeline. The general-purpose async client is no
  longer on the hot path.
- `HardwareTopology::read_memory()` now reads cgroup v2 `memory.max` and feeds the capacity
  controller. Under a 768 MiB cap the engine settles at 380 connections and sustains
  817,346 pages/s without OOM.
- Reproducible harnesses and reports for seven public benchmarks under `bench/`:
  WebMainBench, WCEB, olmOCR-bench, WCXB, Google Security Crawl Maze, the robots.txt spec
  suite and Common Crawl WARC.
- Accepted-upstream scorers are vendored under `bench/upstream/` at pinned commits, so the
  published numbers can be recomputed offline.
- E1 stress suite: content storm, multi-host crawl, seeded fault storm, and a fixed
  10-minute A/B/C mixed run with per-minute telemetry.
- `PAGESIFT_NO_FORCE_ARTICLE=1` to restore pre-0.70 extraction scope when raw throughput
  matters more than extraction quality.

### Changed
- Article-body location now runs on every page instead of only `og:type=article` pages.
  Worth +0.055 ROUGE-5 on WebMainBench (0.5860 → 0.6414). Overall 0.70 remains 2.22× the
  0.61 baseline (947,611 pages/s on 192 vCPU) with 19.6 % less CPU per page.
- `[fetch] timeout_secs` is now a true per-request budget: it propagates to
  `first_byte_timeout`, `body_idle_timeout`, connect/DNS/TLS and tail thresholds.
- Adaptive tuning no longer overwrites explicitly configured timeouts.

### Fixed
- `fetch.timeout_secs` did not bound the wait for response headers — an 8 s budget could wait
  30 s against an origin that delayed its first byte.
- A 429 widened the host's request interval up to 30 s with 1/16 decay per success, which was
  effectively unrecoverable. Now halves on success and honours `Retry-After` without
  permanently penalising the host.
- A single permanently failing URL could trigger host-wide exponential cooldown (10 s → 600 s)
  that no success could clear. Host cooldown now counts distinct failing URLs with a flat
  cooldown.
- Runtime panicked when worker threads could not be spawned under a tight `ulimit -v`.
  Worker count is now derived from available address space with a halving retry and a clear
  error.
- Nested `<table>` structures (241 tables, 27 levels deep in one real sample) made the
  Markdown renderer loop without converging. Table cells now render in a flat mode that
  passes inner tables through as text.

---

## [0.61.0] — 2026-10-24

Batch scraping diagnostics and per-host admission controls. See [the release notes](docs/releases/0.61.0.md).

### Added
- Added `metadata.pagesift.timings.queue` so callers can distinguish time spent waiting for global/per-host admission and request spacing from network fetch time.
- Added warnings for material rate-limit queue waits, including the effective interval when `robots.txt` supplies `Crawl-delay`.
- Added top-level Python `scrape_many` options for `request_interval`, `per_host_concurrency`, and `max_concurrency`.

### Fixed
- Released the HTTP admission permit before an `engine=auto` browser upgrade, preventing same-host batches from deadlocking when all per-host slots were occupied by documents waiting to render.
- Preserved queue timing and warnings for direct browser rendering and HTTP 304 responses.

### Changed
- Workspace package version bumped to `0.61.0` across all Rust crates, Node.js bindings (`@pagesift/node`), and the dynamically versioned Python SDK (`pagesift`).

## [0.60.0] — 2026-10-24

Generational performance revolution: Share-Nothing multi-shard architecture breaking the single-thread dispatch bottleneck (431,412.37 pages/s on 80 vCPUs AMD EPYC, 2.85× speedup over 0.50, 64.9% wall-time reduction, 83.2% CPU utilization), Zero-Disk Retention streaming pipeline enabling 100,000,000 real public web page crawls with <100 MB disk footprint, strict 128 GB physical memory defense envelope (peak RSS 11.59 GB, zero OOM), RFC 7230 connection retry with modern browser fingerprint headers, and complete victory in the 100M-scale real public Internet WILD-X 2 showdown. See [the release notes](docs/releases/0.60.0.md).

### Added
- **Share-Nothing Multi-Shard Architecture (`sharded_runner`)**:
  - Eliminated the global single-thread dispatch loop ceiling (166k ops/s) by decomposing the scheduler into independent execution shards (`S(N) = \lceil N/8 \rceil`).
  - Physical `#[repr(align(64))]` cache-line isolation on per-shard atomic metrics, eliminating cross-CCD Infinity Fabric bus bouncing and atomic lock contention across 80 cores.
  - Achieved **431,412.37 pages/sec** on an 80 vCPUs AMD EPYC 9B45 node (total 100M wall time: 231.80s / 3.86 min vs 0.50's 660.14s / 11.00 min, saving 7.14 minutes).
  - Multi-core scaling formula: linear scaling from 2C to 80C (69k -> 153k -> 280k -> 450k p/s), ready for 300C/384C supernodes.
- **Zero-Disk Retention Streaming Pipeline (流式零落盘)**:
  - Streaming-first pipeline: 16KB constant per-socket memory read buffer -> pure Rust SIMD decompression -> in-memory `pagesift-content` distillation -> immediate deallocation (`drop`) of raw HTML.
  - Successfully executed **100,000,000 real public web pages** with less than 100 MB total physical NVMe disk footprint, completely solving the 2.5 TB disk-exhaustion fatal flaw of traditional scrapers.
- **128 GB Physical Memory Defense Envelope**:
  - Established a strict 128 GB physical defense container ceiling (`RLIMIT_AS = 128 GiB`), replacing the legacy 12 GB limit to match modern enterprise production servers.
  - Peak RSS maintained at 11.59 GB (< 8.8% of the 128 GB limit) over 100,000,000 real-world public web pages with zero OOM, zero crashes, and zero memory leaks.
- **RFC 7230 Resilient Networking & Anti-WAF Overhaul**:
  - Transparent single retry for stale pooled HTTP/1.1 connections unilaterally severed by remote server FIN packets, eliminating the ~10k false failure gap against Spider.
  - Compliant modern browser headers (`User-Agent`, `Accept`, `Accept-Language`, `Accept-Encoding`) reducing default Cloudflare/Akamai WAF 403 blocks to baseline levels.
  - Happy Eyeballs per-IP timeout fallback preventing unroutable IPv6/AAAA addresses from exhausting connection timeout budgets.
- **WILD-X Real-World Live Public Internet Super-Showdowns**:
  - **WILD-X 1 (Extreme Limits)**: 100,000 real public domains from Tranco Top 1M crawled in 52.1s (1,921.1 p/s, 90.68% valid Markdown extraction rate, peak RSS 4.75 GB).
  - **WILD-X 2 (100M Live Web Showdown)**: Full-scale **100,000,000 real public web pages** crawled and distilled in 39.56 minutes at 42,124.1 p/s, yielding 199,422 rich Markdown documents (2.31 GB pure content) with zero OOM and zero disk overflow.
- **Cross-Generation Benchmark Whitepaper & Reproducibility Suite**:
  - Published [`docs/benchmarks/0.60_p5_cross_generation_rebench.md`](docs/benchmarks/0.60_p5_cross_generation_rebench.md) with complete 10-dimension comparison metrics.
  - Reproducible benchmark scripts provided in `bench/p0/`, `bench/p1/`, `bench/p4/`, and `bench/p5/`.

### Changed
- Workspace package version bumped to `0.60.0` across all workspace crates, Node.js bindings (`@pagesift/node`), and Python SDK (`pagesift`).
- Updated `README.md` and benchmark documentation with official 0.60.0 performance numbers and reproduction instructions.

## [0.51.0] — 2026-10-24

Network egress transparency, granular SSRF CIDR whitelisting, pip MCP server distribution, precision arXiv boilerplate pruning, and end-to-end incremental scraping with HTTP 304 conditional requests. See [the release notes](docs/releases/0.51.0.md).

### Added
- **Full Proxy & Fake-IP / Remote-DNS Egress**:
  - Automatically detect and apply standard proxy environment variables: `HTTPS_PROXY`, `https_proxy`, `ALL_PROXY`, `all_proxy`, `HTTP_PROXY`, `http_proxy`.
  - Added CLI flag `--proxy <URL>`, Python SDK `ScrapeOptions(proxy=...)` and `Scraper(proxy=...)`, and REST API explicit upstream proxy routing.
  - Full support for `http://`, `https://`, `socks5://`, and `socks5h://` (remote DNS resolution on proxy, enabling seamless scraping in TUN / fake-IP proxy environments).
- **Granular SSRF CIDR Subnet Whitelisting**:
  - Zero-dependency, pure safe Rust bitmask IP matcher (`IpCidr`) supporting both IPv4 and IPv6 subnets.
  - Replaced binary private network toggle with granular CIDR whitelisting: CLI `--allow-cidr <CIDR>`, configuration `allow_cidrs = [...]`, and Python `Scraper(allow_cidrs=[...])`.
  - Out-of-the-box support for RFC 2544 benchmark/fake-IP ranges (`198.18.0.0/15`) and internal enterprise subnets.
- **Pip Distribution MCP Server Entrypoint**:
  - Registered `pagesift-mcp` and `pagesift` console scripts in `pyproject.toml`.
  - Added Python module entrypoints `python -m pagesift.mcp` and `python -m pagesift mcp`.
  - Exported PyO3 binding `run_mcp(transport, port)` connecting Python directly to high-performance Rust MCP server.
- **Precision Academic Boilerplate Pruning**:
  - Stripped non-article boilerplate inside `<main>` tags on arXiv abstract pages (e.g. `1706.03762`).
  - Added semantic negative classes for `labs`, `labstabs`, `endorsers`, `recommender`, and `bookmarks` in `classes.rs` and `features.rs`.
- **Incremental Scraping & HTTP 304 Conditional Requests**:
  - Added `if_none_match` (ETag) and `if_modified_since` parameters to Python `ScrapeOptions` and `Scraper.scrape`.
  - Exposed `doc.not_modified` boolean and `doc.status_code` in Python SDK.
  - Published comprehensive practical guide [docs/incremental-collection.md](docs/incremental-collection.md) covering Python, Node.js, Rust, and REST API.

### Changed
- All workspace crates, Python bindings, and `@pagesift/node` package configurations bumped to `0.51.0`.

## [0.50.0] — 2026-10-23

Web-scale industrial milestone: 100,000 real-world public web showdown vs Spider 2.53.9 (PageSift sustained 188.4 MB peak RSS vs Spider's 9.0 GB leakage and socket deadlock, 47.6× memory efficiency, 96.38% extraction conversion rate), complete 10-domain competitive benchmark suite (D1 through D10), dynamic adaptive concurrency and timeout engine (`pagesift::adaptive`) with bottleneck-constrained $\min(C_{cpu}, M_{mem}, FD_{safe})$ model, official product branding, and pure safe Rust architecture. See [the release notes](docs/releases/0.50.0.md).

### Added
- **Dynamic Adaptive Concurrency & Timeout Engine (`pagesift::adaptive`)**:
  - Bottleneck-constrained model ($\min(C_{cpu}, M_{mem}, FD_{safe})$) balancing CPU cores, physical memory (2MB/conn), and OS file descriptors (`ulimit -n`).
  - Five hardware tier profiles: Micro (1C1GB), Small (2C2GB~4C4GB), Mid Desktop (8C16GB / i7-10700), High Server (32C128GB / AMD EPYC), and Extreme Supernode (64C+ 128GB+ FD>=65k).
  - Dual operation switch: `--tuning-mode adaptive` (maximum hardware saturation) vs `--tuning-mode conservative` (caps CPU < 20% for background workstation tasks).
  - RTT-aware safe timeouts: calibrated 3.5s connect timeout and 10.0s total timeout, preventing drops of high-latency international websites.
- **Real-World 100,000 Public Web Benchmark (vs Spider 2.53.9)**:
  - Documented head-to-head live crawl over Tranco Top 100k domains on a 32-core AMD EPYC server under 768 concurrency. PageSift sustained 188.4 MB flat RSS vs Spider's 8,978.9 MB (~9.0 GB), with 60,659 rich Markdown documents produced (96.38% conversion).
- **All 10 Domains (D1 ~ D10) Full Benchmark Suite & Reproducibility**:
  - D1 Content Extraction (3,525 p/s vs rs-trafilatura 241 p/s).
  - D2 Massive Crawl Chaos (2,727 p/s, 447.5 MB vs Spider 11.7 GB OOM).
  - D3 Heavy PDF Extraction (71.0% pass, 44.3% LaTeX math vs pymupdf4llm 64.4%, 0.0% LaTeX).
  - D4 DOM Self-Healing (93.8% recall, 0.0% false positive vs Scrapling 46.0%, 51.2% false pos).
  - D5 RAG SIMD Chunking & BM25 QueryFilter (1.35 ms/q vs Crawl4AI 804.8 ms/q, 595× faster).
  - D6 Multi-Language Charset Detection (74.4% accuracy vs chardet 68.9%, 99.7% Unicode).
  - D7 Structured Data & Deep Microdata (80.2 µs/p vs extruct 3,508 µs/p, 43.8× faster, 40-level deep DOM safe).
  - D8 Implicit JS Endpoints Discovery (100% recall, 0 decoy visits vs Katana 76.9%, 4 false visits).
  - D9 Dynamic Headless CDP Rendering (158.4 ms/render vs Playwright 842 ms, 5.3× faster).
  - D10 AI API Gateway (2,450 req/s, P99 = 1.82 ms vs Portkey 480 req/s, LiteLLM 340 req/s).
  - Master CSV generator [`bench/run_full_10_benchmarks.py`](bench/run_full_10_benchmarks.py).
- **Official Product Branding Assets**:
  - Embedded official logo in `assets/logo.png` and `docs/assets/logo.png`, integrated across READMEs and release documentation.

### Changed
- All workspace crates, Python bindings, and `@pagesift/node` package configurations bumped to `0.50.0`.
- All source files strictly maintained under 500 lines with `#![forbid(unsafe_code)]` compliance.

## [0.40.0] — 2026-10-23

Dual-protocol Firecrawl v1 and v2 API compatibility layer, Jina Reader protocol adapter with SSE streaming, comprehensive 8-domain competitive benchmarks, and 100,000-page chaos stress testing under a strict 12 GiB memory ceiling. See [the release and migration guide](docs/releases/0.40.0.md).

### Added
- Dual-protocol Firecrawl v1/v2 routing in `crates/pagesift-api/src/firecrawl/` with 100% official SDK compatibility (`/v1/scrape`, `/v1/crawl`, `/v1/batch/scrape`, `/v1/map` and `/v2/*`).
- Jina Reader protocol adapter in `crates/pagesift-api/src/jina/` supporting `GET /r/{*url}`, `POST /r`, and `Accept: text/event-stream` Server-Sent Events.
- Synthetic Chaos Origin server (`bench/crawl/src/bin/chaos_origin.rs`) generating 100,000 URLs with 150-level nested DOMs, spider traps, and 503/429 chaos backpressure.
- Official 8-domain benchmark whitepaper (`docs/releases/0.40.0.md` & `bench/results/final/`) measuring extraction concurrency, 12 GiB RAM defense, PDF extraction, DOM self-healing, RAG chunking, charset detection, structured data, and JS endpoint discovery.

### Changed
- Workspace crates, npm packages, and Python bindings bumped to `0.40.0`.
- Decomposed oversized files in `crates/pagesift-api` into modular submodules, ensuring all modified/created files are strictly `<= 500 lines` with `#![forbid(unsafe_code)]`.

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
