<div align="center">

# pagesift

**Fast, polite web scraping for LLMs and AI agents.**
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

## Why pagesift

| | |
|---|---|
| ⚡ **Fast** | Main-content extraction in **0.47 ms** per page (p50, single thread): 16× faster than crw, 80× faster than Crawl4AI. Unthrottled HTTP crawling at **3,500+ pages/s** on one machine. |
| 🪶 **Light** | Crawls 200 sites × 100 pages in **~300 MB** where Spider needs 5.5 GB. Replacing Crawl4AI in a production service cut peak memory from 1.4 GB to **225 MB**. |
| 🎯 **Accurate** | Best main-content score on Firecrawl's labelled dataset: **0.733** vs 0.703 (crw) and 0.666 (Crawl4AI), with half the boilerplate leak. Publication dates as accurate as htmldate, the specialist library (F1 **0.948** on its own annotated set), in **0.06 ms** instead of 4 ms. |
| 🧠 **Smart rendering** | Renders only pages that need it, with the lightest engine that works: Lightpanda → moli → Chrome. Learns per site when a browser adds nothing. |
| 🔌 **Drop-in** | Firecrawl v2 API (`/v2/scrape`, `/crawl`, `/map`, `/search`, `/extract`, `/batch/scrape`), so existing Firecrawl SDKs work unchanged. MCP server for agents. |
| 🛡️ **Safe by default** | SSRF protection on every request, redirect, browser sub-request and webhook; DNS-rebinding checks; never disables the browser sandbox. |

Detailed, reproducible numbers: **[docs/benchmarks.md](docs/benchmarks.md)**.

## Quick start

### Docker

```bash
docker run -d -p 3002:3002 -v pagesift-data:/data \
  -e PAGESIFT_API_KEYS=change-me ghcr.io/yolow92/pagesift:0.12

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

### MCP (Claude, Cursor and other agents)

```bash
pagesift mcp            # stdio; tools: pagesift_scrape, pagesift_batch_scrape, pagesift_crawl, pagesift_crawl_status, pagesift_map
```

## Features

**Scraping and crawling**
- Formats: `markdown`, `html` (cleaned), `rawHtml`, `links`, `images`; `summary` and `json` (schema extraction) with any OpenAI-compatible LLM; `changeTracking` (diffs against the previous visit, needs the cache).
- Crawl with include/exclude paths, depth and limits, sitemaps, and robots.txt; **query-focused crawling** (`query`, `adaptive`) visits the most relevant pages first and stops when new pages stop helping.
- Faithful Markdown: tables, nested lists, fenced code with its language, hard line breaks and formulas (KaTeX, MathJax, MathML and Wikipedia math become `$…$` TeX); Markdown characters in the text are escaped only where they would otherwise turn into formatting. Scores 1.000 on a random round-trip test where the next best converter scores 0.936.
- **Crawls real sites without wasting the budget:** strips tracking parameters and session IDs, skips binaries and endless paths, learns which URLs are aliases of the same page, pushes calendar and pagination traps to the back, retries transient failures later and slows down when a site answers 429. On a simulated messy web it finds 74% of the content pages within the page limit, versus 53% for the next best tool (see the benchmarks).
- `queryFilter` keeps only the passages relevant to a query (BM25), to save LLM tokens.
- PDF text extraction; metadata (title, description, language, author, published time, Open Graph). The publication date is searched like htmldate does (meta tags, JSON-LD, date elements, text patterns) with month names in about twenty languages, and keeps the page's own time zone.
- Conditional requests (`ifNoneMatch`, `ifModifiedSince`), a content cache (`maxAge`), content hashes for change detection.

**Rendering**
- Automatic engine choice and fallback (Lightpanda, moli, Chrome/Chromium/chrome-headless-shell), or a remote browser over CDP.
- Adaptive browser pool sized from measured throughput, CPU and free memory; same-origin iframes inlined.
- Ads, trackers, fonts and media blocked by default.

**Running as a service**
- Job persistence and resume after restarts, webhooks with retries and signatures, per-API-key rate, concurrency and daily quotas.
- Prometheus metrics (`/metrics`), JSON logs, graceful shutdown, health checks.
- Named egress proxies per request, host allowlists for internal services, total per-request deadlines, clear error codes (`DNS_FAILURE`, `NETWORK_ERROR`, `BUSY`, `ACCESS_BARRIER`, …).

## How it compares

| | pagesift | Crawl4AI | Firecrawl | crw | Spider |
|---|---|---|---|---|---|
| Language | Rust | Python | TypeScript | Rust | Rust |
| Self-hosted single binary | ✅ | ❌ | ❌ | ✅ | ✅ |
| Firecrawl-compatible API | ✅ | ❌ | ✅ | ✅ | ❌ |
| Main-content extraction score ↑ | **0.733** | 0.666 | — | 0.703 | — |
| Markdown fidelity, random round trip ↑ | **1.000** | 0.787 | — | — | — |
| Extraction p50 per page ↓ | **0.47 ms** | 38.6 ms | — | 7.38 ms | — |
| HTTP crawl, 200 × 100 pages ↑ | **3,500+ p/s** | 15 p/s | — | 206 p/s | 2,000–2,700 p/s |
| Memory for that crawl ↓ | **~300 MB** | — | — | 1.4 GB | 5.5 GB |
| Messy-site crawl: content pages found ↑ | **73.6%** | 52.9% | — | 36.7% | 47.0% |
| Messy-site crawl: wasted requests ↓ | **12%** | 39% | — | 40% | 44% |
| Lightweight JS engines (Lightpanda/moli) | ✅ both, automatic fallback | ❌ | ❌ | Lightpanda | — |
| MCP server | ✅ | ✅ | ✅ | ✅ | — |
| Native Python / Node packages | ✅ / ✅ | ✅ / ❌ | HTTP SDKs | — | ✅ / ✅ |
| License | AGPL-3.0 + commercial | Apache-2.0 | AGPL-3.0 | AGPL-3.0 | MIT |

"—" means not checked. Firecrawl's self-hosted service was not benchmarked. See [docs/benchmarks.md](docs/benchmarks.md) for methods and the full tables.

## Configuration

Everything works with defaults. For a server, copy [`pagesift.example.toml`](pagesift.example.toml) and pass `--config` or set `PAGESIFT_CONFIG`. It covers API keys and limits, webhooks, the User-Agent, rate limits, browser engines, the cache, LLM and search backends, egress proxies and allowed internal hosts.

## Responsible crawling

pagesift respects robots.txt (including `Crawl-delay`), rate-limits every host, backs off on 429 and 5xx, and identifies itself with an honest User-Agent. When a site shows a challenge or access-denied page, pagesift reports it (`ACCESS_BARRIER`) instead of trying to get around it. Pull requests that add stealth, fingerprint impersonation, CAPTCHA solving or proxy rotation are not accepted.

## License

pagesift is dual-licensed:

- **[AGPL-3.0](LICENSE)** for open-source use. Self-host it, modify it, use it commercially; if you offer a modified version to others over a network, share your changes.
- **[Commercial license](COMMERCIAL.md)** for using pagesift without the AGPL's obligations — closed-source products, proprietary hosted services — with optional support.

## Contributing

Issues and pull requests are welcome; please read [CONTRIBUTING.md](CONTRIBUTING.md). Contributors sign a [CLA](CLA.md) once. Security reports: [SECURITY.md](SECURITY.md).
