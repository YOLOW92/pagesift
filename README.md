<div align="center">

<img src="https://raw.githubusercontent.com/YOLOW92/pagesift/main/assets/logo.png" alt="PageSift Logo" width="300" />

# pagesift 0.73.0

**High-performance, memory-bounded web scraping and extraction engine built for AI agents and LLM pipelines.**  
Turn any website, document, or archive into clean Markdown and structured data — one Rust binary, native SDKs, dual Firecrawl v1/v2 & Jina Reader APIs, at a fraction of the memory.

[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](LICENSE)
[![Commercial license](https://img.shields.io/badge/commercial-license%20available-green.svg)](COMMERCIAL.md)
[![Release](https://img.shields.io/github/v/release/YOLOW92/pagesift?include_prereleases)](https://github.com/YOLOW92/pagesift/releases)
[![crates.io](https://img.shields.io/crates/v/pagesift.svg)](https://crates.io/crates/pagesift)
[![PyPI](https://img.shields.io/pypi/v/pagesift.svg)](https://pypi.org/project/pagesift/)
[![npm](https://img.shields.io/npm/v/@pagesift/node.svg)](https://www.npmjs.com/package/@pagesift/node)
[![200M Saturation](https://img.shields.io/badge/200M%20Synthetic-947%2C611%20p%2Fs-blue.svg)](#whats-new-in-0700)
[![768 MiB cgroup](https://img.shields.io/badge/768%20MiB%20cgroup-Zero%20OOM-success.svg)](#whats-new-in-0700)
[![M8ven Score](https://m8ven.ai/badge/mcp/yolow92-pagesift-0ygyrw)](https://m8ven.ai/mcp/yolow92-pagesift-0ygyrw?s=readme)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/esA3wMZVvA)

**English** · [简体中文](docs/README.zh-CN.md) · [日本語](docs/README.ja.md) · [Español](docs/README.es.md)

</div>

---

## What's New in 0.73.0

**All cores again, hardened inputs, fewer false positives.** 0.73.0 fixes what a three-way code
review and ~150 adversarial input tests found.

- **Extraction uses every core again.** Since 0.70.0 `scrape`, `crawl` and `serve` extracted on
  one core; the API server now serves roughly 3.5–7× more requests per second (16 clients, `auto` engine, alternating runs on one i7-10700: 309–336 → 1,153–2,196 req/s, p50 36–45 ms → 4.5–6.1 ms).
- **No more crashes or DoS paths found by the review**: a CJK panic in the feed sniffer, UTF-8
  pages re-decoded into mojibake, quadratic attribute parsing (40,000 attributes: 3 s → 0.02 s),
  slowloris clients, `javascript:` links from RSS items.
- **Heuristics narrowed**: short Substack posts were reported as age gates; author widgets,
  cloaked app roots, `<template>` content and CSS variables (as OpenAI keys) are handled correctly.
- **API limits and timeouts** on every parameter, with the standard JSON error body everywhere.
- **Crawl scope decided from evidence** (`--scope auto`, default): `/docs/python` stays in
  `/docs/python/*`; a leaf seed such as a blog post widens to its section, and says so.

Quality holds or improves (WCXB F1 0.8522 → 0.8533 with WCXB's official scorer on 1,869 of its 2,008 pages — not comparable with the D1 row below, which uses a different scorer; Firecrawl 0.680 → 0.682; AEB 0.958), and extraction is 3–5% faster on 2,786 saved real pages (one machine, alternating runs). Full detail:
[**0.73.0 release notes**](docs/releases/0.73.0.md).

## What's New in 0.72.8

**Truthful signals, complete allow-lists, better extraction.** 0.72.8 closes the fourth-round
audit and the fifth-to-eighth-round field audits.

- 🔤 **Charset headers are verified** — a lying `Content-Type` no longer produces silent
  mojibake; Pagesift re-decodes the body and reports every correction in
  `metadata.pagesift.warnings`.
- 🌐 **`--allow-cidr` everywhere** — `crawl`, `map`, `download`, `session-fetch`, `collect` and
  `serve` now accept the same private-network allow-list as `scrape`; invalid CIDRs fail loudly.
- ⚙️ **Default config discovery** — `--config` > `PAGESIFT_CONFIG` > the platform config file
  (`~/.config/pagesift/pagesift.toml`, `$XDG_CONFIG_HOME`, or `%APPDATA%` on Windows).
  `PAGESIFT_CONFIG=` disables config files for Docker/CI.
- 🩺 **`doctor` tells the truth** — it prints the config file that was actually loaded, or the
  default path to create.
- 🚦 **No more fake success** — `map` returns the seed error instead of a one-URL result, and
  `crawl` exits 1 when it collected zero pages and at least one error.
- 🍪 **Recipes keep the recipe** — content asides such as ingredient lists are no longer pruned;
  preview blocks (`quickview`/`teaser`) no longer replace the real article.
- 💻 **GitHub code pages are code again** — line numbers are dropped and code rows are merged
  into one fenced block.
- 👥 **Authors survive** — arXiv-style author lists stay in the body, and `citation_author`
  fills `metadata.author`.
- 🧱 **Barriers say so** — German/French/Spanish block pages are `ACCESS_BARRIER`, age gates are
  `interstitial`, and thin extraction emits `thin_content`.
- 📡 **Feeds are parsed** — RSS/Atom become Markdown item lists; `jsonLd` is passed through.
- 🖼️ **Image titles and video metadata** — `title` attributes are kept, and video pages fall
  back to their JSON-LD `VideoObject` instead of returning an empty document.

Full detail: [**0.72.8 release notes**](docs/releases/0.72.8.md).

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
- 📈 **Scaling measured 1 → 192 vCPU on a synthetic local origin, fitted with the Universal Scalability Law** — γ = 8,631.8 pages/s/core, σ = 1.34 %, κ fitted to ≈ 0, R² = 0.839. The fit is a rough description only: its own report ([`0.70_p1_cpu_scaling_usl.md`](docs/benchmarks/0.70_p1_cpu_scaling_usl.md)) says it must not be extrapolated.
- 🧠 **Memory is now an input.** The engine reads cgroup v2 `memory.max` and sizes itself to it: at **768 MiB** it settles on 380 connections and still sustains **817,346 pages/s** with zero OOM.
- 🏁 **200M saturation on 192 vCPU (synthetic local origin, no network — not a real-internet speed)** — `200,000,000 / 200,000,000`, **0 failures**, **947,611 pages/s**, 211.1 s wall, **888.1 MiB peak RSS**. Source: [`0.70_200m_regression.md`](docs/benchmarks/0.70_200m_regression.md).
- 📊 **Seven public benchmarks, published including the losses** — WebMainBench **0.6414** ROUGE-5 (level with Trafilatura 0.6402 and above Resiliparse 0.6290, but below Readability 0.6543 and magic-html 0.7138), WCEB **0.8613**, WCXB **0.8472** (official scorer, 1,869 pages), robots.txt spec **0.9621**, olmOCR-bench **0.4172**, Common Crawl **5,165 p/s**, Crawl Maze **0.0500**. Details and caveats: [baselines 1–4](docs/benchmarks/0.70_p4_step4_2_baselines_1_4.md), [baselines 5–7](docs/benchmarks/0.70_p4_step4_2_baselines_5_7.md).
- 🔧 **Four production-grade defects fixed**, all found by the stress suite: header-wait ignoring `timeout_secs`, 429 penalties that never recovered, one broken URL freezing a whole host, and a runtime panic under tight `ulimit -v`.
- ⚖️ **Faster and cheaper per page than 0.61, on the synthetic origin**: **2.22×** absolute throughput (947,611 p/s on 192 vCPU), **+24.4 % per core**, and **−19.6 % CPU per page** — while also lifting WebMainBench ROUGE-5 by **+0.055** (the narrower main-content selection behind that gain lowered WCXB F1 from 0.6824 to 0.6652, see D1 below). Per-core output on a 16-core box sits within 4.8 % of the 192-core run.

Full detail: [**0.70.0 release notes**](docs/releases/0.70.0.md).

---

## 🏆 Seven-Domain Public Benchmark Showdown (0.70 — measured October 2026, not re-run for later versions)

Every number below is a **full-set run on the same machine**, with the suite's own scorer, under a
**10 GiB memory cap**. Competitors run in the same containers. Where we lose, the loss is printed.

Measured on **Intel i7-10700 (8C/16T), 15 GB RAM, Ubuntu 26.04**. Reproduce with
[`bench/scripts/`](bench/scripts/) — every report links to the exact binaries, dataset checksums and
scorer commits.

| # | Domain | PageSift 0.70 | Best competitor | Rank | Peak RAM |
| :---: | :--- | :--- | :--- | :---: | ---: |
| **D1** | Content extraction (WCXB 2,006) | 0.6652 F1 | 0.6824 (PageSift 0.50) | 4th | 751 MiB |
| **D3** | PDF extraction (olmOCR 1,403 / 7,019 checks) | **0.4506** | 0.2446 (pymupdf4llm) | **1st** | 271 MiB |
| **D5** | RAG chunking + BM25 (472 questions) | **0.9470** R@5 (its best strategy; `recursive` scores 0.9004) | 0.9364 (semchunk) | **1st** | — |
| **D6** | Charset detection (3,128 files) | **0.7602** | 0.7113 (chardet) | **1st** | 39 MiB |
| **D7** | Structured data (2,006 pages) | **0.7392** | 0.5488 (bs4+lxml) | **1st** | **21 MiB** |
| **D9** | Dynamic rendering (7 SPA scenarios) | 3/7 | 5/7 (Playwright) | 2nd | — |
| **D4** | DOM self-healing (2,000 mutations) | 0.8215 | 0.8875 (Scrapling) | 2nd | — |

**Seven domains measured, four won.** The three we did not win are reported as-is:

- **D1**: we trade recall for precision — 0.8938 precision (second of 17 entrants, behind goose3's 0.9097) vs 0.5851 recall. WCXB's non-article reference text is whole-page content, so whole-page output scores better on this metric; PageSift 0.70 also scores 0.017 F1 below PageSift 0.50 here.
- **D9**: we render the two real SPA scenarios (client-side and hydration) correctly, at 0.46–0.50 s per page; the Playwright figure (about 2.55 s) includes a fixed 2 s wait added by the test script, so it is not a like-for-like speed comparison. We don't wait for `setTimeout`-delayed injections.
- **D4**: rank 2nd overall, but we win outright on adversarial decoys (100 % vs 86.4 %). The gap
  comes from a category where about 14.6 % of the cases' labels are wrong (our own inspection of this synthetic mutation set) (a "price" slot contains a
  person's name).

Speed where it matters: **1,403 PDFs in 0.83 s** (PageSift on 16 threads vs Python tools run as 16 shards: 31× faster than pypdf, ~290× faster than pymupdf4llm), **3,128 encoding detections at 11,239 files/s**, **21 MiB peak** for structured data.

> **Where the numbers come from**: full reports under [`docs/benchmarks/0.70_showdown_D*.md`](docs/benchmarks/)
> and harnesses under [`bench/scripts/`](bench/scripts/). Raw per-run outputs (`out/`) are not stored in the repository,
> so a re-run needs the public datasets and the scripts.

---

## ⚔️ Historical run (0.60, October 2026): 100,000,000 requests against the live internet

> **Read this as a stress test, not a "100 million pages" result.** Historical data, **not re-run for
> the current version**. Source: [`docs/benchmarks/0.60_p4_wild_x_2_showdown.md`](docs/benchmarks/0.60_p4_wild_x_2_showdown.md);
> runner: [`bench/p4/run_wild_x2.py`](bench/p4/run_wild_x2.py) and `bench/p4/src/wild_x2_runner.rs`. The raw
> run log is not in the repository.

**Setup**: Google Cloud `n4d-highcpu-80` (80 vCPU AMD EPYC 9B45, 157 GiB RAM), Tranco top-1M domains combined with a fixed list of
canonical paths, **100,000,000 requests issued** over 4,096 concurrent sockets in 32 share-nothing shards.

| Metric | Result |
| :--- | :--- |
| Requests issued / wall time | 100,000,000 / 2,373.9 s (42,124 requests/s; peak > 51,900) |
| **Real `200 OK` responses** | **314,170 (0.31 %)** — 199,422 of them yielded ≥ 100 bytes of Markdown (2.31 GB) |
| Other outcomes in the report | 205,856 × 403, 615,232 DNS errors, 60,730 TLS errors, 244,243 timeouts (together ≈ 1.44 M requests) |
| Not broken down in the report | the remaining ≈ 98.6 M requests (the runner files them under 4xx/5xx and other network errors) |
| Peak RSS | 11.59 GB (limit set to 128 GB), no OOM |
| Disk growth | < 100 MB (HTML is parsed from memory and discarded) |

So the request rate says how fast pagesift fails and moves on as much as how fast it extracts: only about 0.3 % of the
requests returned a page. What the run does show is that a 100M-request run stays within memory and disk bounds and does not crash.

> **Synthetic throughput, same hardware (0.60)**: 431,412 pages/s over 100,000,000 *synthetic* pages from a local origin
> (0.50: 151,483 pages/s). No network is involved. See [`0.60_p5_cross_generation_rebench.md`](docs/benchmarks/0.60_p5_cross_generation_rebench.md).

---

## Quick Start

### 1. Docker / Self-Hosted Server

Run the standalone container (includes headless browser renderer):

```bash
docker run -d -p 3002:3002 -v pagesift-data:/data \
  -e PAGESIFT_API_KEYS=sk-test ghcr.io/yolow92/pagesift:0.73.0
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

# Extraction itself takes about 1–2 ms per page (median, network excluded)
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
pagesift crawl "https://docs.rs" --limit 5000 --tuning-mode adaptive --out ./data/

# Config is discovered automatically from ~/.config/pagesift/pagesift.toml
# (or $XDG_CONFIG_HOME / %APPDATA%). PAGESIFT_CONFIG= disables config files.
pagesift doctor
```

### 5. Shared MCP Server for AI Agents

```bash
# One local service for every agent on the machine (127.0.0.1 only)
pagesift mcp http --port 3003 --audit-log ~/.local/state/pagesift/audit.jsonl
claude mcp add --scope user --transport http pagesift http://127.0.0.1:3003/mcp
```

Shared browsers and cache, an audit line per fetch, HTML snapshots of suspicious results and a
`pagesift_report_issue` tool. See [Running pagesift as a shared MCP service](docs/mcp-service.md).

---

## Why PageSift? (mixed-vintage numbers, see the notes below the table)

| Feature | PageSift (version and source in the notes) | Firecrawl (Self-Hosted) | Crawl4AI | Spider |
| :--- | :---: | :---: | :---: | :---: |
| **Core Architecture** | Rust; `forbid(unsafe_code)` in every crate except `pagesift-sys` (one glibc `mallopt` call) | TypeScript / Python / Redis | Python / Playwright | Rust / Tokio |
| **API Compatibility** | **Firecrawl v1 + v2 + Jina Reader + MCP** | Firecrawl API only | Custom Python API | Custom REST API |
| **Extraction Throughput** | 3,525 pages/sec (0.40 era, 16 threads on WCXB) · 59,309 (16 vCPU) / 947,611 (192 vCPU) on a synthetic local origin ¹ | ~150 pages/sec ² | ~90 pages/sec ² | ~1,200 pages/sec ² |
| **Peak RAM (100k Domains)** | **188.4 MB** (0.50) | > 8 GB ² | > 10 GB ² | 8,978 MB (measured; Spider was also 7.1× faster there, 666.6 vs 93.5 QPS) ³ |
| **Share of `200 OK` pages with ≥ 100 B of Markdown** | 96.38% (0.50) | ~85% ² | ~88% ² | 82.82% ³ |
| **Adaptive Hardware Engine** | **$\min(CPU, RAM, FD)$ Auto** | Manual worker count | Manual semaphore | Fixed semaphore |
| **DOM Self-Healing** | 93.8% on a 500-case set (0.50); 0.8215 recall on a 2,000-case set (0.70, below Scrapling's 0.8875) ⁴ | ❌ No | ❌ No | ❌ No |
| **PDF Extraction & Math** | 71.0% / 44.3% LaTeX on a 1,438-page subset (0.50); 0.4506 on the full olmOCR-bench (0.70) ⁴ | External docx/pdf text | PDF raw text only | Raw text only |
| **RAG Chunking & BM25** | 19.7 ms chunking / 1.35 ms BM25 filter (0.50) | ❌ No | Python BM25 filter 804 ms | ❌ No |
| **Anti-Bot Philosophy** | Ethical, polite, transparent UA | Proxy rotation / bypass | Stealth evasions | Stealth evasions |

¹ Synthetic origin on the same machine, no network ([`0.70_200m_regression.md`](docs/benchmarks/0.70_200m_regression.md)); the competitor figures are not comparable to it.
² No measurement for this figure exists in this repository: an estimate kept from an earlier README, **not verified**. Measured comparisons are in [`docs/benchmarks.md`](docs/benchmarks.md) and [`docs/benchmarks/`](docs/benchmarks/).
³ [`BENCHMARK_100K_PAGESIFT_VS_SPIDER.md`](bench/results/final/BENCHMARK_100K_PAGESIFT_VS_SPIDER.md) (0.50, one run, 100k domains). The percentage is the share of pages for which each tool's own extractor returned ≥ 100 bytes, not a fidelity score.
⁴ Different datasets and versions in the same cell; the 0.70 numbers are the newer same-machine runs ([D4](docs/benchmarks/0.70_showdown_D4.md), [D3](docs/benchmarks/0.70_showdown_D3.md)). Competitor columns for Firecrawl and Crawl4AI were not measured on these rows.

---

## Architectural Highlights

- **Segmented Bounded Frontier:** Queues are bounded, so memory does not grow with crawl depth (188.4 MB peak over 105,660 sites in the 0.50 100k run).
- **Dynamic Hardware Adaptive Tuning:** Detects system resources and calculates the safe theoretical saturation point without crashing or exhausting sockets.
- **Selective Rendering Pipeline:** Automatically detects static vs dynamic pages; executes headless Chrome *only* when dynamic JavaScript hydration provides net content gains.
- **SSRF & Sandbox Defense:** Strict private IP filtering, DNS-rebinding guards, and perpetual browser sandboxing block private-range targets by default (design goals, covered by tests; not an audit).
- **Ethical Politeness:** Per-host token buckets, automatic robots.txt compliance, and RFC-compliant User-Agent identification.

---

## Community

Questions or feedback? Join us on [Discord](https://discord.gg/esA3wMZVvA). The upcoming hosted API (closed beta coming soon, not yet open) will be discussed there too.

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
  note   = {Version 0.73.0}
}
```

---

## ⭐ Star History

[![Star History Chart](https://api.star-history.com/svg?repos=YOLOW92/pagesift&type=Date)](https://star-history.com/#YOLOW92/pagesift&Date)
