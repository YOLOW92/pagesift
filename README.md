<div align="center">

# pagesift 0.40.0

**High-performance, memory-bounded web scraping and extraction engine built for AI agents and LLM pipelines.**  
Turn any website, document, or archive into clean Markdown and structured data — one Rust binary, native SDKs, dual Firecrawl v1/v2 & Jina Reader APIs, at a fraction of the memory.

[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](LICENSE)
[![Commercial license](https://img.shields.io/badge/commercial-license%20available-green.svg)](COMMERCIAL.md)
[![Release](https://img.shields.io/github/v/release/YOLOW92/pagesift?include_prereleases)](https://github.com/YOLOW92/pagesift/releases)
[![crates.io](https://img.shields.io/crates/v/pagesift.svg)](https://crates.io/crates/pagesift)
[![PyPI](https://img.shields.io/pypi/v/pagesift.svg)](https://pypi.org/project/pagesift/)
[![npm](https://img.shields.io/npm/v/@pagesift/node.svg)](https://www.npmjs.com/package/@pagesift/node)
[![Memory Defense](https://img.shields.io/badge/12GiB%20RAM%20Defense-Verified%20OOM--Proof-success.svg)](#benchmark-showdown)

**English** · [简体中文](docs/README.zh-CN.md) · [日本語](docs/README.ja.md) · [Español](docs/README.es.md)

</div>

---

## 🏆 Benchmark Showdown (All 8 Domains)

All benchmarks run on bare-metal **Intel Core i7-10700** (8 cores, 16 threads, 15.5 GB RAM) under an active **12 GiB memory watchdog** (`MemoryMax=12288M`). Verified raw CSVs available in [`bench/results/final/`](bench/results/final/).

| Domain | PageSift 0.40.0 | Primary Competitor | Advantage / Speedup | Peak RAM (PageSift vs Competitor) |
| :--- | :--- | :--- | :--- | :---: |
| **01. Content Extraction** | **3,525 pages/s** (0.57s) | `rs-trafilatura`: 241 p/s (8.32s)<br>`trafilatura` (Py): 191 p/s (10.51s) | **14.6× ~ 18.4× faster** | **137.8 MB** vs 298.3 MB |
| **02. 50k Crawl & 12G Defense** | **2,727 pages/s** (17.76s)<br>**447.5 MB** peak RSS | `Spider 2.53.9`: 11.7 GB RSS<br>→ **KILLED BY OOM (12GB Limit)** | **Zero memory leaks**<br>(1/26 the RAM) | **447.5 MB** vs 11,724.8 MB (💥 OOM) |
| **03. PDF Extraction (olmOCR)** | **71.0%** pass, **44.3%** LaTeX<br>**3.4 ms/page** | `pymupdf4llm`: 64.4% pass, 0.0% LaTeX<br>320 ms/page | **94× faster**<br>Native LaTeX math recovery | **42.0 MB** vs 280.0 MB |
| **04. DOM Self-Healing** | **93.8%** recall, **0.0%** false pos<br>**24.6 µs** latency | `Scrapling`: 46.0% recall, 51.2% false pos<br>635.3 µs latency | **25.8× faster**, zero false pos<br>200-deep DOM safe (928 µs) | **39.4 MB** vs 98.0 MB |
| **05. RAG Chunking & BM25** | R@1000 = **0.895** (19.7 ms)<br>BM25 QueryFilter: **1.35 ms/q** | `semchunk`: 113.4 ms (5.8× slower)<br>`Crawl4AI`: 804.8 ms/q (**595× slower**) | **5.8× chunking speedup**<br>**595× BM25 filter speedup** | **45.2 MB** vs 340.0 MB |
| **06. Charset Detection** | **74.4%** accuracy, **99.7%** Unicode<br>**0.45 ms/file** | `chardet 7.7`: 68.9% acc (3.05 ms)<br>`charset-normalizer`: 46.5% acc (1.05 ms) | **Higher accuracy (+5.5%)**<br>**6.8× faster** | **18.5 MB** vs 42.0 MB |
| **07. Structured Data** | **80.2 µs/page** (standard)<br>40-level deep nesting: **363 µs** | `extruct 0.18`: 3,508 µs/page (43.8× slower)<br>40-level deep nesting: 15,921 µs | **43.8× ~ 43.9× faster**<br>Zero stack exhaustion | **24.0 MB** vs 85.0 MB |
| **08. Implicit JS Discovery** | **13/13 (100.0%)** recall<br>**0 non-nav visits**, **0.42 ms** | `Katana 1.8.0`: 10/13 (76.9%)<br>4 non-nav visits (30.8% false), 48.5 ms | **100% recall**, zero decoy visits<br>Zero browser sandbox overhead | **14.2 MB** vs 128.0 MB |

> [!TIP]
> **Machine-verifiable datasets**: Read the complete benchmark whitepaper at [`docs/releases/0.40.0.md`](docs/releases/0.40.0.md) and examine all raw CSV records in [`bench/results/final/`](bench/results/final/).

---

## What's New in 0.40.0

- 🌐 **Dual-Protocol Firecrawl v1 & v2 Architecture:** Drop-in compatibility with official Firecrawl Python and Node.js SDKs (`V1FirecrawlApp` and `FirecrawlApp`) covering `/v1/scrape`, `/v1/crawl`, `/v1/batch/scrape`, `/v1/map` and all `/v2/*` endpoints.
- ⚡ **Jina Reader Protocol Adapter:** Instant URL scraping via `GET /r/{*url}` or `POST /r`, with Server-Sent Events (`Accept: text/event-stream`) streaming for real-time LLM agent feeds.
- 🛡️ **12 GiB RAM Defense Under Chaos:** In a 50,000-page adversarial live crawl, `Spider` crashed from OOM (11.7 GB RSS), while PageSift completed 48,433 pages at **2,727 pages/s** in **447.5 MB** RSS. Survived 100,000 synthetic chaos URLs with 150-level nested DOMs and 3,000 503 backpressure spikes.
- 🦀 **100% Pure Safe Rust & Modular Architecture:** `#![forbid(unsafe_code)]` preserved across `pagesift-api`. All modified and created `.rs` source files strictly `<= 500 lines`.

---

## Quick Start

### 1. Docker / Self-Hosted Server

Run the standalone container (includes headless browser renderer):

```bash
docker run -d -p 3002:3002 -v pagesift-data:/data \
  -e PAGESIFT_API_KEYS=sk-test ghcr.io/yolow92/pagesift:0.40.0
```

#### Firecrawl SDK Drop-In Replacement
Point your existing Firecrawl application directly to PageSift — zero code refactoring:

```python
from firecrawl import FirecrawlApp

# Drop-in local PageSift instance
app = FirecrawlApp(api_url="http://localhost:3002", api_key="sk-test")
res = app.scrape_url("https://example.com", params={"formats": ["markdown"]})
print(res["markdown"])
```

#### Jina Reader Drop-In Replacement
Point any LLM Agent or simple HTTP client to PageSift's `/r/` endpoint:

```bash
# Direct Markdown output
curl -s http://localhost:3002/r/https://en.wikipedia.org/wiki/Rust_(programming_language)

# Streamed SSE output
curl -s -N -H "Accept: text/event-stream" http://localhost:3002/r/https://news.ycombinator.com
```

---

### 2. Python Package

```bash
pip install pagesift
```

```python
import pagesift

# Single-page fast extraction (< 2ms)
page = pagesift.scrape("https://example.com", formats=["markdown", "metadata"])
print(page.markdown)
print(page.metadata.title, page.metadata.published_date)

# Batch scraping with bounded memory
results = pagesift.scrape_many(["https://site1.com", "https://site2.com"], concurrency=8)
```

---

### 3. Node.js / TypeScript Package

```bash
npm install @pagesift/node
# or
npm install pagesift
```

```typescript
import { scrape, scrapeMany } from '@pagesift/node'

// Scrape page to clean LLM markdown
const page = await scrape('https://example.com', {
  formats: ['markdown', 'links']
})
console.log(page.markdown)

// Concurrent stream processing
const stream = scrapeMany(['https://a.com', 'https://b.com'], { concurrency: 4 })
for await (const page of stream) {
  console.log(`Scraped: ${page.url} in ${page.elapsedMs}ms`)
}
```

---

### 4. Command Line Interface (CLI)

```bash
# Install standalone binary via cargo
cargo install pagesift --locked

# Scrape single URL
pagesift scrape "https://news.ycombinator.com" --format markdown

# High-concurrency polite crawl
pagesift crawl "https://docs.rs" --limit 5000 --concurrency 16 --output ./data/
```

---

## Why PageSift?

| Feature | PageSift 0.40.0 | Firecrawl (Self-Hosted) | Crawl4AI | Spider |
| :--- | :---: | :---: | :---: | :---: |
| **Core Architecture** | Pure Safe Rust (`forbid(unsafe)`) | TypeScript / Python / Redis | Python / Playwright | Rust / Tokio |
| **API Compatibility** | **Firecrawl v1 + v2 + Jina Reader + MCP** | Firecrawl API only | Custom Python API | Custom REST API |
| **Extraction Throughput** | **3,525 pages/sec** | ~150 pages/sec | ~90 pages/sec | ~1,200 pages/sec |
| **Peak RAM (50k Pages)** | **447.5 MB (Bounded)** | > 6 GB (Redis queues) | > 8 GB (Browser pool) | 💥 **OOM Crashed (11.7 GB)** |
| **DOM Self-Healing** | **93.8% (0.0% False Pos)** | ❌ No | ❌ No | ❌ No |
| **PDF Extraction & Math** | **71% Pass / 44.3% LaTeX** | External docx/pdf text | PDF raw text only | Raw text only |
| **RAG Chunking & BM25** | **19.7 ms / 1.35 ms BM25** | ❌ No | Python chunkers (804 ms) | ❌ No |
| **Anti-Bot Philosophy** | Ethical, polite, transparent UA | Proxy rotation / bypass | Stealth evasions | Stealth evasions |

---

## Architectural Highlights

- **Segmented Bounded Frontier:** Memory does not scale with crawl depth. Even with 100,000 URLs, backpressure and bounded queues prevent out-of-memory disasters.
- **Selective Rendering Pipeline:** Automatically detects static vs dynamic pages; executes headless Chrome *only* when dynamic JavaScript hydration provides net content gains.
- **SSRF & Sandbox Defense:** Strict private IP filtering, DNS-rebinding guards, and perpetual browser sandboxing ensure zero local network exposure.
- **Ethical Politeness:** Per-host token buckets, automatic robots.txt compliance, and RFC-compliant User-Agent identification.

---

## License & Commercial Options

- **Open Source:** Licensed under [AGPL-3.0](LICENSE).
- **Commercial Licensing:** Need proprietary linking or private deployment without AGPL obligations? Check out our [Commercial Licensing Guide](COMMERCIAL.md) or contact `licensing@pagesift.dev`.
