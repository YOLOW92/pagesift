<div align="center">

<img src="https://raw.githubusercontent.com/YOLOW92/pagesift/main/assets/logo.png" alt="PageSift Logo" width="300" />

# pagesift 0.71.0

**High-performance, memory-bounded web scraping and extraction engine built for AI agents and LLM pipelines.**  
Turn any website, document, or archive into clean Markdown and structured data — one Rust binary, native SDKs, dual Firecrawl v1/v2 & Jina Reader APIs, at a fraction of the memory.

[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](LICENSE)
[![Commercial license](https://img.shields.io/badge/commercial-license%20available-green.svg)](COMMERCIAL.md)
[![Release](https://img.shields.io/github/v/release/YOLOW92/pagesift?include_prereleases)](https://github.com/YOLOW92/pagesift/releases)
[![crates.io](https://img.shields.io/crates/v/pagesift.svg)](https://crates.io/crates/pagesift)
[![PyPI](https://img.shields.io/pypi/v/pagesift.svg)](https://pypi.org/project/pagesift/)
[![npm](https://img.shields.io/npm/v/@pagesift/node.svg)](https://www.npmjs.com/package/@pagesift/node)
[![200M Saturation](https://img.shields.io/badge/200M%20Synthetic-947%2C611%20p%2Fs-blue.svg)](#whats-new-in-0700)
[![128GB Memory Safe](https://img.shields.io/badge/128GB%20Envelope-Zero%20OOM-success.svg)](#whats-new-in-0700)
[![M8ven Score](https://m8ven.ai/badge/mcp/yolow92-pagesift-0ygyrw)](https://m8ven.ai/mcp/yolow92-pagesift-0ygyrw?s=readme)

**English** · [简体中文](docs/README.zh-CN.md) · [日本語](docs/README.ja.md) · [Español](docs/README.es.md)

</div>

---

## What's New in 0.71.0

**A bug-fix release for the 0.70.0 field audit.** The extraction engine and API contract are
unchanged; failures are now reported honestly and authorization applies consistently.

- 🧭 **Crawl scope fixed** — `/index.html`-style seeds cover the whole site instead of one page.
- 🔐 **Proxy and allow-lists** — `NO_PROXY` is honored; `--allow-cidr` / `allow_hosts` also apply
  to browser rendering, not just HTTP.
- 🧾 **Honest content reporting** — binary responses return `UNSUPPORTED_OPTION`, empty pages
  carry `empty_content`, and bilibili captcha pages return `ACCESS_BARRIER`.
- 🔒 **TLS errors are classified** — `TLS_CERT_EXPIRED`, `TLS_HOSTNAME_MISMATCH`,
  `TLS_UNTRUSTED_ROOT` and `TLS_ERROR` replace the generic `NETWORK_ERROR`.
- 🩺 **Root and auto fallback explained** — `doctor` flags the root sandbox problem, and
  `--engine auto` prints why rendering was unavailable.
- 🚦 **Exit codes match page status** — HTTP 5xx and empty 404 responses exit with code 2.
- 🧹 **Hidden templates filtered** — `x-cloak` and similar pre-hydration templates no longer
  leak into Markdown.
- 📚 **Charset handling unified** — `convert` detects local file charsets and accepts `--charset`.

Full detail: [**0.71.0 release notes**](docs/releases/0.71.0.md).

---

## What's New in 0.70.0

**The engine underneath the numbers was replaced.** 0.60 proved PageSift could finish 100,000,000 tasks; 0.70 rebuilds the data plane that does it and puts the whole system under one closed-loop controller.

- ⚙️ **Hand-written NUMA-local HTTP data plane** — fixed-slot NUMA executors, a minimal HTTP/1.1 reactor, hybrid reactor↔content scheduling, per-origin adaptive connections and sharded intake with bounded work stealing. The general-purpose async client is gone from the hot path.
- 📈 **Scaling measured 1 → 192 vCPU, fitted with the Universal Scalability Law** — γ = 8,631.8 pages/s/core, σ = 1.34 %, **κ = 0.00 %** (no measurable coherency penalty), R² = 0.839.
- 🧠 **Memory is now an input.** The engine reads cgroup v2 `memory.max` and sizes itself to it: at **768 MiB** it settles on 380 connections and still sustains **817,346 pages/s** with zero OOM.
- 🏁 **200M saturation on 192 vCPU** — `200,000,000 / 200,000,000`, **0 failures**, **947,611 pages/s**, 211.1 s wall, **888.1 MiB peak RSS**.
- 📊 **Seven public benchmarks, published including the losses** — WebMainBench **0.6414** ROUGE-5 (level with Trafilatura, above Resiliparse), WCEB **0.8613**, WCXB **0.8472**, robots.txt spec **0.9621**, olmOCR-bench **0.4172**, Common Crawl **5,165 p/s**, Crawl Maze **0.0500**.
- 🔧 **Four production-grade defects fixed**, all found by the stress suite: header-wait ignoring `timeout_secs`, 429 penalties that never recovered, one broken URL freezing a whole host, and a runtime panic under tight `ulimit -v`.
- ⚖️ **Faster and cheaper per page than 0.61**: **2.22×** absolute throughput (947,611 p/s on 192 vCPU), **+24.4 % per core**, and **−19.6 % CPU per page** — while also lifting WebMainBench ROUGE-5 by **+0.055**. Per-core output on a 16-core box sits within 4.8 % of the 192-core run.

Full detail: [**0.70.0 release notes**](docs/releases/0.70.0.md).

---

## 🏆 Seven-Domain Public Benchmark Showdown (0.70)

Every number below is a **full-set run on the same machine**, with the suite's own scorer, under a
**10 GiB memory cap**. Competitors run in the same containers. Where we lose, the loss is printed.

Measured on **Intel i7-10700 (8C/16T), 15 GB RAM, Ubuntu 26.04**. Reproduce with
[`bench/scripts/`](bench/scripts/) — every report links to the exact binaries, dataset checksums and
scorer commits.

| # | Domain | PageSift 0.70 | Best competitor | Rank | Peak RAM |
| :---: | :--- | :--- | :--- | :---: | ---: |
| **D1** | Content extraction (WCXB 2,006) | 0.6720 F1 | 0.6824 (PageSift 0.50) | 4th | 751 MiB |
| **D3** | PDF extraction (olmOCR 1,403 / 7,019 checks) | **0.4506** | 0.2446 (pymupdf4llm) | **1st** | 271 MiB |
| **D5** | RAG chunking + BM25 (472 questions) | **0.9470** R@5 | 0.9364 (semchunk) | **1st** | — |
| **D6** | Charset detection (3,128 files) | **0.7602** | 0.7113 (chardet) | **1st** | 39 MiB |
| **D7** | Structured data (2,006 pages) | **0.7392** | 0.5488 (bs4+lxml) | **1st** | **21 MiB** |
| **D9** | Dynamic rendering (7 SPA scenarios) | 3/7 | 5/7 (Playwright) | 2nd | — |
| **D4** | DOM self-healing (2,000 mutations) | 0.8215 | 0.8875 (Scrapling) | 2nd | — |

**Seven domains measured, four won.** The three we did not win are reported as-is:

- **D1**: we trade recall for precision — 0.8854 precision (highest of all 17 entrants) vs 0.5989 recall.
  WCXB's reference text includes boilerplate, so whole-page output "wins" on this metric.
- **D9**: we render SPAs correctly (client-side and hydration both hit) and finish in **0.47 s vs
  Playwright's 2.55 s — 5× faster** — but we don't wait for `setTimeout`-delayed injections.
- **D4**: rank 2nd overall, but we win outright on adversarial decoys (100 % vs 86.4 %). The gap
  comes from a category where 14.6 % of the dataset's labels are wrong (a "price" slot contains a
  person's name).

Speed where it matters: **1,403 PDFs in 0.83 s** (31× faster than pypdf, 296× faster than
pymupdf4llm), **3,128 encoding detections at 11,239 files/s**, **21 MiB peak** for structured data.

> **Machine-verifiable**: full reports under [`docs/benchmarks/0.70_showdown_D*.md`](docs/benchmarks/),
> raw outputs under `out/` (gitignored), harnesses under [`bench/scripts/`](bench/scripts/).

---

## ⚔️ Flagship Showdown: 100,000,000 Live Public Internet Battle

**Hardware Testbed**: **Google Cloud `n4d-highcpu-80` (80 vCPUs AMD EPYC 9B45, 157 GiB RAM, 96 GB NVMe SSD)**
**Target Dataset**: Tranco Top 1,000,000 Global Live Domains dynamically expanded to **100,000,000 Real Public Web Pages** under 4,096 concurrent sockets across 32 Share-Nothing shards.

```text
+-----------------------------------------------------------------------------------------+
| PageSift 0.60 WILD-X 2 Showdown (100,000,000 Real Public Web Pages on 80-Core 157GB VPS) |
+-----------------------------------------------------------------------------------------+
| Metric                       | PageSift 0.60 Performance                                |
+------------------------------+----------------------------------------------------------+
| Total Web Pages Processed    | 100,000,000 (One Hundred Million) Pages                 |
| Total Execution Wall Time    | 2,373.9 seconds (39.56 minutes / 0.66 hours)            |
| Sustained Live Public QPS    | 42,124.1 pages/sec (Peak > 51,900 p/s)                   |
| 128 GB Memory Defense        | Peak RSS: 11.59 GB (flat line, 8.8% of 128GB ceiling)   |
| Zero-Disk Retention Delta    | Physical NVMe disk increase < 100 MB (85 GB free)        |
| Genuine 200 OK Raw Fetches   | 314,170 pages (real-world diverse public web)           |
| Pure Rich Markdown Distilled | 199,422 documents (2.31 GB pure content, zero garbage)   |
| Resilience & Auto-Recovery   | 615k DNS errs, 244k timeouts, 205k WAF 403s handled     |
| Multi-Million Scalability    | 100% OOM-Proof, Zero Leaks, Zero Hangs, Zero Panics      |
+-----------------------------------------------------------------------------------------+
```

> [!IMPORTANT]
> **Key 0.60 Breakthroughs for Web-Scale AI Training:**
> - **Zero-Disk Retention Streaming**: 100,000,000 pages parsed through a 16KB constant memory buffer with zero disk accumulation (< 100 MB disk delta), completely bypassing the 2.5 TB disk-exhaustion fatal flaw of traditional scrapers.
> - **431,412 pages/sec Pure Throughput**: Tested end-to-end on synthetic multi-modal datasets, cutting 100M wall time from 11.00 minutes to **3.86 minutes (2.85× speedup)**.
> - **128 GB Physical Wall Defense**: Maintained flat 11.59 GB peak RSS over 100M pages, strictly honoring enterprise cloud memory quotas with zero OOM.
> - Read the official whitepapers at [`docs/benchmarks/0.60_p4_wild_x_2_showdown.md`](docs/benchmarks/0.60_p4_wild_x_2_showdown.md) and [`docs/benchmarks/0.60_p5_cross_generation_rebench.md`](docs/benchmarks/0.60_p5_cross_generation_rebench.md).

---

## Quick Start

### 1. Docker / Self-Hosted Server

Run the standalone container (includes headless browser renderer):

```bash
docker run -d -p 3002:3002 -v pagesift-data:/data \
  -e PAGESIFT_API_KEYS=sk-test ghcr.io/yolow92/pagesift:0.71.0
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

| Feature | PageSift 0.70.0 | Firecrawl (Self-Hosted) | Crawl4AI | Spider |
| :--- | :---: | :---: | :---: | :---: |
| **Core Architecture** | Pure Safe Rust (`forbid(unsafe)`) | TypeScript / Python / Redis | Python / Playwright | Rust / Tokio |
| **API Compatibility** | **Firecrawl v1 + v2 + Jina Reader + MCP** | Firecrawl API only | Custom Python API | Custom REST API |
| **Extraction Throughput** | **59,309 pages/sec (16 vCPU)** · **947,611 (192 vCPU)** | ~150 pages/sec | ~90 pages/sec | ~1,200 pages/sec |
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

---

## 🗺️ Roadmap

- [x] NUMA-local HTTP data plane (0.70)
- [x] cgroup-aware adaptive capacity (0.70)
- [x] Seven public extraction benchmarks with reproducible harnesses (0.70)
- [ ] Resumable crawl checkpoints — restart without re-fetching
- [ ] OCR fallback for scanned documents
- [ ] Resource-URL enumeration mode for security crawlers
- [ ] Distributed shard coordination across nodes

Vote or propose: [Issues](https://github.com/YOLOW92/pagesift/issues) · [Discussions](https://github.com/YOLOW92/pagesift/discussions)

---

## 📚 Citation

If PageSift is useful in research or a product, cite it as:

```bibtex
@software{pagesift2026,
  title  = {PageSift: NUMA-Local Rust Web Extraction Engine for AI Pipelines},
  author = {YOLOW92},
  year   = {2026},
  url    = {https://github.com/YOLOW92/pagesift},
  note   = {Version 0.71.0}
}
```

---

## ⭐ Star History

[![Star History Chart](https://api.star-history.com/svg?repos=YOLOW92/pagesift&type=Date)](https://star-history.com/#YOLOW92/pagesift&Date)
