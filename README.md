<div align="center">

# pagesift 0.20.0

**Web content, incremental collections and verified archives. Built in Rust.**
Turn any website into clean Markdown and structured data — one Rust binary, a Firecrawl-compatible API, and a fraction of the memory.

[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](LICENSE)
[![Commercial license](https://img.shields.io/badge/commercial-license%20available-green.svg)](COMMERCIAL.md)
[![Release](https://img.shields.io/github/v/release/YOLOW92/pagesift?include_prereleases)](https://github.com/YOLOW92/pagesift/releases)
[![crates.io](https://img.shields.io/crates/v/pagesift.svg)](https://crates.io/crates/pagesift)
[![PyPI](https://img.shields.io/pypi/v/pagesift.svg)](https://pypi.org/project/pagesift/)
[![npm](https://img.shields.io/npm/v/pagesift.svg)](https://www.npmjs.com/package/pagesift)

**English** · [简体中文](docs/README.zh-CN.md) · [日本語](docs/README.ja.md) · [Español](docs/README.es.md)

</div>

---

pagesift is a self-hosted scraping and crawling engine for AI pipelines, RAG and agents. It fetches pages, renders JavaScript only when it has to, extracts the main content, and returns Markdown, HTML, links and metadata over a [Firecrawl v2](https://docs.firecrawl.dev)-compatible HTTP API, an MCP server, a CLI, and native Python and Node packages.

It is built for running as a service: bounded memory, backpressure, job persistence, webhooks, per-key limits and metrics. It also plays by the rules — robots.txt, per-host rate limits, an honest User-Agent, and no anti-bot bypassing.

## New in 0.20.0

![Pagesift 0.20.0 engine overview](docs/assets/overview.svg)

Native Rust cores now cover explicit HTTP sessions, stable item identities and incremental checkpoints, verified media archives, fuzzy candidates, forms, external modules and finite HLS/DASH VOD. New collection and archiving operations are exposed through Rust APIs and CLI commands; Python and Node retain their documented scraping surface.

**[Release and upgrade guide](docs/releases/0.20.0.md)** · **[Documentation](docs/README.md)** · **[Architecture diagrams](docs/architecture.md)**

## Why pagesift

| | |
|---|---|
| ⚡ **Fast** | Main-content extraction in **0.85 ms** per page (p50, single thread): 10× faster than crw, 59× faster than Crawl4AI. Unthrottled HTTP crawling at **3,800+ pages/s** on one machine. |
| 🪶 **Light** | Crawls 200 sites × 100 pages in **~220 MB** where Spider needs 4.1 GB. Replacing Crawl4AI in a production service cut peak memory from 1.4 GB to **225 MB**. |
| 🎯 **Accurate** | Best main-content score on Firecrawl's labelled dataset: **0.716** vs 0.711 (crw) and 0.670 (Crawl4AI), with half the boilerplate leak. Publication dates as accurate as htmldate, the specialist library (F1 **0.948** on its own annotated set), in **0.06 ms** instead of 4 ms. |
| 🧠 **Smart rendering** | Renders only pages that need it, with the lightest engine that works: Lightpanda → moli → Chrome. Learns per site when a browser adds nothing. |
| 🔌 **Drop-in** | Firecrawl v2 API (`/v2/scrape`, `/crawl`, `/map`, `/search`, `/extract`, `/batch/scrape`), so existing Firecrawl SDKs work unchanged. MCP server for agents. |
| 🛡️ **Safe by default** | SSRF protection on every request, redirect, browser sub-request and webhook; DNS-rebinding checks; never disables the browser sandbox. |

Detailed, reproducible numbers: **[docs/benchmarks.md](docs/benchmarks.md)**.

## Quick start

### Docker

```bash
docker run -d -p 3002:3002 -v pagesift-data:/data \
  -e PAGESIFT_API_KEYS=change-me ghcr.io/yolow92/pagesift:0.20.0

curl -s http://localhost:3002/v2/scrape \
  -H 'Authorization: Bearer change-me' -H 'Content-Type: application/json' \
  -d '{"url": "https://example.com", "formats": ["markdown", "links"]}'
```

The image includes Lightpanda for JavaScript rendering.

### Command line

```bash
# from a release binary (Linux, macOS) or with Cargo
cargo install pagesift

pagesift scrape https://example.com                 # Markdown to stdout
pagesift crawl https://example.com --out ./site     # one Markdown file per page
pagesift map https://example.com                    # list a site's URLs
pagesift serve --port 3002                          # the HTTP API
pagesift doctor                                     # check browsers, network, config
```

### Python

```bash
pip install pagesift
```

```python
import pagesift

doc = pagesift.scrape("https://example.com")
print(doc.markdown)

# many pages at once, on every core
for doc in pagesift.scrape_iter(["https://example.com/a", "https://example.com/b"]):
    print(doc.metadata["title"])
```

### Node.js

```bash
npm install pagesift
```

```js
const { Scraper } = require('pagesift')

const scraper = new Scraper()
const page = await scraper.scrape('https://example.com')
console.log(page.markdown)
```

### MCP

```bash
pagesift mcp            # stdio; tools: pagesift_scrape, pagesift_batch_scrape, pagesift_crawl, pagesift_crawl_status, pagesift_map
```

## Features

**Scraping and crawling**
- Formats: `markdown`, `html` (cleaned), `rawHtml`, `links`, `images`; `summary` and `json` (schema extraction) with any OpenAI-compatible LLM; `changeTracking` (diffs against the previous visit, needs the cache).
- Rust business anchors relocate uniquely identified items across selector changes and reject missing or duplicate identities. Semantic item discovery and `pagesift collect` add JSON API pagination checkpoints, updates, opt-in deletion sync and a WordPress adapter; see [incremental collection](docs/incremental-collection.md).
- The Rust store provides explicit source/item/revision completion records for incremental collectors. Callers supply stable identities and commit after delivery; this is separate from automatic URL crawling and cache identity.
- Link discovery includes anchors, frames, HTMX GET, pagination and HTTP `Link` headers, plus bounded static inline navigation and literal GET calls. Dynamic script expressions and unknown/write options are excluded; see [script discovery](docs/script-discovery.md).
- `pagesift download` archives direct file URLs by source/item/revision with streamed disk writes, validated Range/ETag resume, BLAKE3 verification and recoverable atomic completion; see [media archive](docs/media-archive.md). Explicit site adapters, bounded finite HLS/DASH and named metadata publication are available; broad ecosystem parity remains incomplete. See [native core boundaries](docs/native-core.md).
- `pagesift session-fetch` adds explicit, origin-isolated HTTP Cookie sessions with private persisted state, expiry/use budgets and sticky retirement; see [HTTP sessions](docs/http-sessions.md). Browser sessions and automatic authenticated crawl integration remain open work.
- Crawl with include/exclude paths, depth and limits, sitemaps, and robots.txt; **query-focused crawling** (`query`, `adaptive`) visits the most relevant pages first and stops when new pages stop helping.
- Faithful Markdown: tables, nested lists, fenced code with its language, hard line breaks and formulas (KaTeX, MathJax, MathML and Wikipedia math become `$…$` TeX); Markdown characters in the text are escaped only where they would otherwise turn into formatting. Scores 1.000 on a random round-trip test where the next best converter scores 0.937.
- **Crawls real sites without wasting the budget:** strips tracking parameters and session IDs, skips binaries and endless paths, learns which URLs are aliases of the same page, pushes calendar and pagination traps to the back, retries transient failures later and slows down when a site answers 429. On a simulated messy web it finds 71% of the content pages within the page limit, versus 49% for the next best tool (see the benchmarks).
- `queryFilter` keeps only the passages relevant to a query: the page is split into ~100-token units inside its sections, scored with BM25 together with their heading path, and units scoring at least half of the best are kept, in page order with their headings. On a labelled RAG benchmark it keeps 93% of the answer text in about 1,150 words, where Crawl4AI's BM25 filter keeps 67% in 13,500 words, and it is 73× faster. `maxTokens`, `topK` and an absolute `threshold` are available.
- `chunking` returns the Markdown as chunks ready for RAG, each with its heading path and position: `recursive` (size-bounded, splits at the most meaningful boundary, keeps code blocks and headings with their content; a Rust port of semchunk, with the same retrieval quality and over 3× faster), headings, paragraphs, sentences and token windows.
- PDF to Markdown with tables and inline formulas as LaTeX (olmOCR-bench overall 71.0 vs 64.4 for pymupdf4llm, formulas 44% vs 0%, at 3.6 ms per page); web pages in any encoding (all 32 non-UTF-8 pages of a random sample read correctly, where crw and Crawl4AI garble 13); metadata (title, description, language, author, published time, Open Graph). The publication date is searched like htmldate does (meta tags, JSON-LD, date elements, text patterns) with month names in about twenty languages, and keeps the page's own time zone.
- Conditional requests (`ifNoneMatch`, `ifModifiedSince`), a content cache (`maxAge`), content hashes for change detection.

**Rendering**
- Automatic engine choice and fallback (Lightpanda, moli, Chrome/Chromium/chrome-headless-shell), or a remote browser over CDP.
- Adaptive browser pool sized from measured throughput, CPU and free memory; same-origin iframes inlined.
- Ads, trackers, fonts and media blocked by default.

**Running as a service**
- Job persistence and resume after restarts, webhooks with retries and signatures, per-API-key rate, concurrency and daily quotas.
- Prometheus metrics (`/metrics`), JSON logs, graceful shutdown, health checks.
- Named egress proxies per request, host allowlists for internal services, total per-request deadlines, clear error codes (`DNS_FAILURE`, `NETWORK_ERROR`, `BUSY`, `ACCESS_BARRIER`, …).

## Six-project evidence for 0.20.0

![545-page diagnostic extraction quality](docs/assets/extraction-quality.svg)

The frozen pre-release candidate was compared against pinned Firecrawl, Crawlee, Scrapling, Katana, Essence and gallery-dl versions. All 545 calibrated WebMainBench pages are retained in the two quality comparisons above. This is a diagnostic token-multiset F1, **not the official leaderboard**; component and complete-service URL contexts are separate. Pagesift still loses on 117 Essence component pages and 96 Firecrawl service pages.

![Controlled task comparisons](docs/assets/controlled-tasks.svg)

These synthetic tasks test disclosed modes: Crawlee Cheerio, Scrapling adaptive selectors and 8 Mastodon posts / 20 originals. They do not establish complete-framework or site-wide leadership.

![Forty-category capability coverage](docs/assets/capability-coverage.svg)

**17 categories remain partial.** Persistent browser-interaction and autonomous Agent workflows are outside this release's work scope. See [public protocols and per-page scores](bench/six/evidence/README.md), [full comparison](bench/six/REPORT.md) and [40-category inventory](bench/six/CAPABILITIES.md).

## Earlier benchmark comparisons

| | pagesift | Crawl4AI | Firecrawl | crw | Spider |
|---|---|---|---|---|---|
| Language | Rust | Python | TypeScript | Rust | Rust |
| Self-hosted single binary | ✅ | ❌ | ❌ | ✅ | ✅ |
| Firecrawl-compatible API | ✅ | ❌ | ✅ | ✅ | ❌ |
| Main-content extraction score ↑ | **0.716** | 0.670 | — | 0.711 | — |
| Markdown fidelity, random round trip ↑ | **1.000** | 0.784 | — | — | — |
| RAG query filter: answer kept / words returned | **93% / 1,150** | 67% / 13,500 | — | — | — |
| Extraction p50 per page ↓ | **0.85 ms** | 50.2 ms | — | 8.93 ms | — |
| HTTP crawl, 200 × 100 pages ↑ | **3,890 p/s** | 17 p/s | — | 48 p/s | 2,090 p/s |
| Memory for that crawl ↓ | **218 MB** | 946 MB | — | 447 MB | 4.1 GB |
| Messy-site crawl: content pages found ↑ | **71.0%** | 49.1% | — | 35.4% | 34.7% |
| Messy-site crawl: wasted requests ↓ | **13%** | 44% | — | 45% | 45% |
| Lightweight JS engines (Lightpanda/moli) | ✅ both, automatic fallback | ❌ | ❌ | Lightpanda | — |
| MCP server | ✅ | ✅ | ✅ | ✅ | — |
| Native Python / Node packages | ✅ / ✅ | ✅ / ❌ | HTTP SDKs | — | ✅ / ✅ |
| License | AGPL-3.0 + commercial | Apache-0.20.0 | AGPL-3.0 | AGPL-3.0 | MIT |

"—" means not checked in these earlier tables. See [docs/benchmarks.md](docs/benchmarks.md) for their methods. The newer [version-locked six-project report](bench/six/REPORT.md) includes a complete self-hosted Firecrawl quality replay and explicit remaining gaps; it does not establish full-feature leadership.

## Configuration

Additional native Rust tools for fuzzy element candidates, form inspection, external JavaScript discovery and media/stream archiving are documented in [docs/native-core.md](docs/native-core.md), including their supported subsets and remaining gaps. These additions do not establish full competitor parity.

Everything works with defaults. For a server, copy [`pagesift.example.toml`](pagesift.example.toml) and pass `--config` or set `PAGESIFT_CONFIG`. It covers API keys and limits, webhooks, the User-Agent, rate limits, browser engines, the cache, LLM and search backends, egress proxies and allowed internal hosts.

## Responsible crawling

pagesift respects robots.txt (including `Crawl-delay`), rate-limits every host, backs off on 429 and 5xx, and identifies itself with an honest User-Agent. When a site shows a challenge or access-denied page, pagesift reports it (`ACCESS_BARRIER`) instead of trying to get around it. Pull requests that add stealth, fingerprint impersonation, CAPTCHA solving or proxy rotation are not accepted.

## License

pagesift is dual-licensed:

- **[AGPL-3.0](LICENSE)** for open-source use. Self-host it, modify it, use it commercially; if you offer a modified version to others over a network, share your changes.
- **[Commercial license](COMMERCIAL.md)** for using pagesift without the AGPL's obligations — closed-source products, proprietary hosted services — with optional support.

## Contributing

Issues and pull requests are welcome; please read [CONTRIBUTING.md](CONTRIBUTING.md). Contributors sign a [CLA](CLA.md) once. Security reports: [SECURITY.md](SECURITY.md).
