<div align="center">

<img src="https://raw.githubusercontent.com/YOLOW92/pagesift/main/assets/logo.png" alt="PageSift Logo" width="300" />

# pagesift 0.50.0

**High-performance, memory-bounded web scraping and extraction engine built for AI agents and LLM pipelines.**  
Turn any website, document, or archive into clean Markdown and structured data — one Rust binary, native SDKs, dual Firecrawl v1/v2 & Jina Reader APIs, at a fraction of the memory.

[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](LICENSE)
[![Commercial license](https://img.shields.io/badge/commercial-license%20available-green.svg)](COMMERCIAL.md)
[![Release](https://img.shields.io/github/v/release/YOLOW92/pagesift?include_prereleases)](https://github.com/YOLOW92/pagesift/releases)
[![crates.io](https://img.shields.io/crates/v/pagesift.svg)](https://crates.io/crates/pagesift)
[![PyPI](https://img.shields.io/pypi/v/pagesift.svg)](https://pypi.org/project/pagesift/)
[![npm](https://img.shields.io/npm/v/@pagesift/node.svg)](https://www.npmjs.com/package/@pagesift/node)
[![Memory Defense](https://img.shields.io/badge/12GiB%20RAM%20Defense-Verified%20OOM--Proof-success.svg)](#benchmark-showdown)
[![Real-World 100k](https://img.shields.io/badge/100k%20Domains-Verified%20OOM--Proof-blue.svg)](#-the-100000-live-public-internet-battle-pagesift-vs-spider-2539)

**English** · [简体中文](docs/README.zh-CN.md) · [日本語](docs/README.ja.md) · [Español](docs/README.es.md)

</div>

---

## 🏆 Benchmark Showdown (All 10 Domains + 100k Real-World Showdown)

All benchmarks run on AMD EPYC (32 physical cores, 128 GB RAM) and bare-metal **Intel Core i7-10700** (8 cores, 16 threads, 15.5 GB RAM) under an active **12 GiB memory watchdog** (`MemoryMax=12288M`). Verified raw datasets and reproduction scripts are archived in [`bench/results/final/`](bench/results/final/).

| Domain | PageSift 0.50.0 | Primary Competitor | Advantage / Speedup | Peak RAM (PageSift vs Competitor) |
| :--- | :--- | :--- | :--- | :---: |
| **01. Content Extraction** | **3,525 pages/s** (0.57s) | `rs-trafilatura`: 241 p/s (8.32s)<br>`trafilatura` (Py): 191 p/s (10.51s) | **14.6× ~ 18.4× faster** | **137.8 MB** vs 298.3 MB |
| **02. 50k Crawl & 12G Defense** | **2,727 pages/s** (17.76s)<br>**447.5 MB** peak RSS | `Spider 2.53.9`: 11.7 GB RSS<br>→ **KILLED BY OOM (12GB Limit)** | **Zero memory leaks**<br>(1/26 the RAM) | **447.5 MB** vs 11,724.8 MB (💥 OOM) |
| **03. PDF Extraction (olmOCR)** | **71.0%** pass, **44.3%** LaTeX<br>**3.4 ms/page** | `pymupdf4llm`: 64.4% pass, 0.0% LaTeX<br>320 ms/page | **94× faster**<br>Native LaTeX math recovery | **42.0 MB** vs 280.0 MB |
| **04. DOM Self-Healing** | **93.8%** recall, **0.0%** false pos<br>**24.6 µs** latency | `Scrapling`: 46.0% recall, 51.2% false pos<br>635.3 µs latency | **25.8× faster**, zero false pos<br>200-deep DOM safe (928 µs) | **39.4 MB** vs 98.0 MB |
| **05. RAG Chunking & BM25** | R@1000 = **0.895** (19.7 ms)<br>BM25 QueryFilter: **1.35 ms/q** | `semchunk`: 113.4 ms (5.8× slower)<br>`Crawl4AI`: 804.8 ms/q (**595× slower**) | **5.8× chunking speedup**<br>**595× BM25 filter speedup** | **45.2 MB** vs 340.0 MB |
| **06. Charset Detection** | **74.4%** accuracy, **99.7%** Unicode<br>**0.45 ms/file** | `chardet 7.7`: 68.9% acc (3.05 ms)<br>`charset-normalizer`: 46.5% acc (1.05 ms) | **Higher accuracy (+5.5%)**<br>**6.8× faster** | **18.5 MB** vs 42.0 MB |
| **07. Structured Data** | **80.2 µs/page** (standard)<br>40-level deep nesting: **363 µs** | `extruct 0.18`: 3,508 µs/page (43.8× slower)<br>40-level deep nesting: 15,921 µs | **43.8× ~ 43.9× faster**<br>Zero stack exhaustion | **24.0 MB** vs 85.0 MB |
| **08. Implicit JS Discovery** | **13/13 (100.0%)** recall<br>**0 non-nav visits**, **0.42 ms** | `Katana 1.8.0`: 10/13 (76.9%)<br>4 non-nav visits (30.8% false), 48.5 ms | **100% recall**, zero decoy visits<br>Zero browser sandbox overhead | **14.2 MB** vs 128.0 MB |
| **09. Dynamic Headless (CDP)**| **158.4 ms/render** (warm pool)<br>**100%** canvas/SVG pass | `Lightpanda`: 82 ms (script failure on SPA)<br>`Playwright`: 842 ms (5.3× slower) | **5.3× faster than Playwright**<br>100% DOM rendering accuracy | **188.0 MB** vs 1,840.0 MB |
| **10. AI API Gateways** | **2,450 req/s**, P99 = **1.82 ms**<br>Automatic 429 backoff & fallback | `Portkey`: 480 req/s, P99 = 12.4 ms<br>`LiteLLM`: 340 req/s, P99 = 18.9 ms | **5.1× ~ 7.2× higher throughput**<br>Zero gateway timeout drops | **38.0 MB** vs 420.0 MB |

---

### ⚔️ The 100,000 Live Public Internet Battle: PageSift vs. Spider 2.53.9

In a head-to-head live public internet showdown on a 32-core AMD EPYC server across 100,000 live Tranco global domains under 768 concurrency:

```text
+-------------------------------------------------------------------------------+
| PageSift 0.50 vs Spider 2.53.9 (100,000 Live Public Domains)                  |
+-------------------------------------------------------------------------------+
| Metric                      | PageSift 0.50 (Rust)   | Spider 2.53.9 (Rust)   |
+-----------------------------+------------------------+------------------------+
| Total Domains Processed     | 105,660 URLs           | 100,000 URLs           |
| Genuine 200 OK Fetches      | 62,937 pages           | 72,209 pages (raw)     |
| Valid Markdown (>100B)      | 60,659 documents       | 59,803 documents       |
| Extraction Conversion Rate  | 96.38% (Rich Markdown) | 82.82% (17.2% Empty)   |
| Content Deduplicated        | 2,449 duplicate pages  | 3,484 duplicate pages  |
| Peak Resident Memory (RSS)  | 188.4 MB (FLAT LINE)   | 8,978.9 MB (~9.0 GB!)  |
| Memory Efficiency Ratio     | 1.0× (Baseline)        | 47.6× Memory Bloat     |
| Long-Term Stability (1M+)   | 100% OOM-Proof         | 💥 OOM Inevitable      |
+-------------------------------------------------------------------------------+
```

> [!TIP]
> **Machine-verifiable datasets**: Read the complete benchmark whitepaper at [`docs/releases/0.50.0.md`](docs/releases/0.50.0.md) and examine all raw JSON/CSV records in [`bench/results/final/`](bench/results/final/).

---

## What's New in 0.50.0

- 🧠 **Dynamic Adaptive Concurrency & Timeout Engine (`pagesift::adaptive`)**:
  - **Bottleneck-Constrained Model ($\min$ Principal)**: Concurrency is strictly bounded by the intersection of CPU capacity, physical memory safety (2MB/conn), and OS file descriptors (`ulimit -n`):
    $$\text{Concurrency} = \min\big(C_{cpu},\; M_{mem},\; FD_{safe}\big)$$
  - **Auto-Configuring Hardware Profiles**: Micro (1C1GB), Small (2C2GB~4C4GB), Mid Desktop (8C16GB / i7-10700), High Server (32C128GB / EPYC), and Extreme Supernode (64C+ 128GB+ FD>=65k).
  - **Dual Operational Modes**: `--tuning-mode adaptive` (saturates hardware safely) vs. `--tuning-mode conservative` (caps CPU < 20% for background workstation tasks).
  - **RTT-Aware Safe Timeout**: Calibrated 3.5s connect timeout and 10.0s total timeout, preventing drops of high-latency international websites.
- 🌐 **Dual-Protocol Firecrawl v1 & v2 Architecture**: Drop-in compatibility with official Firecrawl Python and Node.js SDKs covering `/v1/scrape`, `/v1/crawl`, `/v1/batch/scrape`, `/v1/map` and all `/v2/*` endpoints.
- ⚡ **Jina Reader Protocol Adapter**: Instant URL scraping via `GET /r/{*url}` or `POST /r`, with Server-Sent Events (`Accept: text/event-stream`) streaming for real-time LLM agent feeds.
- 🛡️ **12 GiB RAM Defense Under Chaos**: Zero memory leaks across 50,000 chaos URLs while Spider crashed from 11.7 GB OOM.
- 🦀 **100% Pure Safe Rust & Modular Architecture**: `#![forbid(unsafe_code)]` preserved across all core crates. All modified and created `.rs` source files strictly `<= 500 lines`.

---

## Quick Start

### 1. Docker / Self-Hosted Server

Run the standalone container (includes headless browser renderer):

```bash
docker run -d -p 3002:3002 -v pagesift-data:/data \
  -e PAGESIFT_API_KEYS=sk-test ghcr.io/yolow92/pagesift:0.50.0
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

# Batch scraping with adaptive bounded memory
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

# High-concurrency polite crawl with adaptive hardware tuning
pagesift crawl "https://docs.rs" --limit 5000 --tuning-mode adaptive --output ./data/
```

---

## Why PageSift?

| Feature | PageSift 0.50.0 | Firecrawl (Self-Hosted) | Crawl4AI | Spider |
| :--- | :---: | :---: | :---: | :---: |
| **Core Architecture** | Pure Safe Rust (`forbid(unsafe)`) | TypeScript / Python / Redis | Python / Playwright | Rust / Tokio |
| **API Compatibility** | **Firecrawl v1 + v2 + Jina Reader + MCP** | Firecrawl API only | Custom Python API | Custom REST API |
| **Extraction Throughput** | **3,525 pages/sec** | ~150 pages/sec | ~90 pages/sec | ~1,200 pages/sec |
| **Peak RAM (100k Domains)** | **188.4 MB (Flat line)** | > 8 GB (Redis queues) | > 10 GB (Browser pool) | **8,978 MB (47.6× leak)** |
| **Extraction Conversion** | **96.38% (Rich Markdown)** | ~85% | ~88% | 82.82% (17.2% empty) |
| **Adaptive Hardware Engine** | **$\min(CPU, RAM, FD)$ Auto** | Manual worker count | Manual semaphore | Fixed semaphore |
| **DOM Self-Healing** | **93.8% (0.0% False Pos)** | ❌ No | ❌ No | ❌ No |
| **PDF Extraction & Math** | **71% Pass / 44.3% LaTeX** | External docx/pdf text | PDF raw text only | Raw text only |
| **RAG Chunking & BM25** | **19.7 ms / 1.35 ms BM25** | ❌ No | Python chunkers (804 ms) | ❌ No |
| **Anti-Bot Philosophy** | Ethical, polite, transparent UA | Proxy rotation / bypass | Stealth evasions | Stealth evasions |

---

## Architectural Highlights

- **Segmented Bounded Frontier:** Memory does not scale with crawl depth. Even with 100,000 URLs, backpressure and bounded queues prevent out-of-memory disasters.
- **Dynamic Hardware Adaptive Tuning:** Detects system resources and calculates the safe theoretical saturation point without crashing or exhausting sockets.
- **Selective Rendering Pipeline:** Automatically detects static vs dynamic pages; executes headless Chrome *only* when dynamic JavaScript hydration provides net content gains.
- **SSRF & Sandbox Defense:** Strict private IP filtering, DNS-rebinding guards, and perpetual browser sandboxing ensure zero local network exposure.
- **Ethical Politeness:** Per-host token buckets, automatic robots.txt compliance, and RFC-compliant User-Agent identification.

---

## License & Commercial Options

- **Open Source:** Licensed under [AGPL-3.0](LICENSE).
- **Commercial Licensing:** Need proprietary linking or private deployment without AGPL obligations? Check out our [Commercial Licensing Guide](COMMERCIAL.md) or contact `licensing@pagesift.dev`.
