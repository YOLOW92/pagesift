# Benchmarks

Every number here comes from scripts in [`bench/`](../bench) and can be rerun. Each metric says which direction is better. Tools see exactly the same inputs; timing runs use one tool at a time on an otherwise idle machine.

**Machine:** Intel Core i7-10700 (8 cores / 16 threads), 15 GB RAM, NVMe SSD, Ubuntu (Linux 7.0). **Date:** 2026-10-21. **pagesift:** 0.12.0.

> Status: these are the measurements made during development. A full rerun of every section with the release build is planned before the release and will replace these tables.

## 1. Main-content extraction: quality and speed

**Dataset:** the 597 HTML pages of Firecrawl's `scrape-content-dataset-v1` that have both an HTML snapshot and labelled truth/noise phrases (news, blogs, docs, shops, forums; many languages). Pages whose HTTP extraction is thin use a browser-rendered snapshot instead (92 pages), for every tool. Inputs are frozen in `bench/data/inputs`; adapters are in [`bench/compare`](../bench/compare).

**Metrics:**
- *recall*: share of labelled main-content phrases found in the output (higher is better);
- *leak*: share of labelled noise phrases (menus, footers, cookie banners, related links) that made it into the output (lower is better);
- *score*: F1 of recall and 1 − leak (higher is better);
- *found*: pages where at least 30% of truth phrases are found, crw's own metric (higher is better);
- *p50 / p95*: single-thread processing time per page, excluding I/O (lower is better).

| Tool | Score ↑ | Recall ↑ | Leak ↓ | Found ↑ | p50 ms ↓ | p95 ms ↓ |
|---|---|---|---|---|---|---|
| **pagesift 0.12** | **0.733** | 0.594 | 0.044 | 66.8% | **0.47** | **2.0** |
| crw (crw-extract) | 0.703 | 0.574 | 0.092 | 64.8% | 7.38 | 36.0 |
| Crawl4AI 0.9 (fit markdown) | 0.666 | 0.532 | 0.108 | 61.8% | 38.6 | 157 |
| Mozilla Readability + Turndown | 0.606 | 0.448 | 0.062 | 52.6% | 174 | 1170 |
| trafilatura | 0.596 | 0.430 | **0.030** | 51.3% | 21.2 | 136 |
| Defuddle | 0.473 | 0.605 | 0.612 | 67.0% | 874 | 4347 |
| MarkItDown | 0.464 | **0.618** | 0.628 | 67.3% | 40.4 | 145 |
| Crawl4AI 0.9 (raw markdown) | 0.459 | 0.605 | 0.631 | **67.5%** | 38.6 | 157 |
| dom_smoothie | 0.329 | 0.199 | 0.048 | 25.5% | 3.18 | 14.2 |

Tools with high recall and ~60% leak (MarkItDown, Defuddle, Crawl4AI raw) convert the whole page rather than extracting the main content. pagesift processes **6,489 pages/s** on 16 threads (mean page 285 KB).

Reproduce: see [`bench/compare/README.md`](../bench/compare/README.md).

## 2. Crawling over HTTP (no rate limit)

**Setup:** a local multi-site origin ([`bench/crawl`](../bench/crawl)) serving real pages from the corpus on `s{i}.localhost`, with 10 ms of added latency; every tool crawls every site with its own link discovery, unthrottled. Memory is the peak proportional set size (PSS) of the tool's whole process tree.

| Scenario | Tool | Throughput, pages/s ↑ | Peak memory ↓ |
|---|---|---|---|
| wide: 200 sites × 100 pages | **pagesift** | **3,500–4,400** | **280–316 MB** |
| | Spider (Rust) | 2,000–2,700 | 5.5 GB |
| | crw | 206 | 1.4 GB |
| | Crawl4AI (1 process) | 15 | — |
| | Crawl4AI (multi-process) | 42 | 7.8 GB |
| huge: 1,000 sites × 50 pages | **pagesift** | **~3,300** | **~390 MB** |
| | crw | — | 2.2 GB |
| | Spider | — | > 9.5 GB (stopped by the memory watchdog) |

Reproduce: `bench/crawl/run_all.sh`.

## 3. Browser rendering

pagesift drives browsers over its own CDP client and picks an engine automatically: **Lightpanda** first, **moli** when a page needs iframes or more complete JavaScript, **Chrome / chrome-headless-shell** last. Same crawl setup as above, every page rendered.

| Engine | wide pages/s ↑ | huge pages/s ↑ | Peak memory ↓ |
|---|---|---|---|
| **pagesift auto (adaptive pool)** | **~80–95** | **~82–91** | 0.5–0.9 GB |
| Lightpanda only | 70.9 | 65.1 | ~0.48 GB |
| chrome-headless-shell | 33 | 34 | 1.4–1.5 GB |
| moli | 28.8 | 29.6 | 0.84–0.9 GB |
| Google Chrome | 11.6 | 11.7 | 2.2–2.4 GB |

Reproduce: `bench/crawl/engines.sh` (set `ENGINES=auto` for the default chain).

## 4. A production workload: replacing Crawl4AI

[AiSeek](https://aiseek.dev) fetches news and company pages with Crawl4AI 0.9.4. We ran its own benchmark (`bench.py`) against both services with the production server's limits (6 cores, 10 GB memory, CPU weight 25), on 210 real production URLs (205 hosts) and 25 fixed pages. pagesift ran behind a small adapter implementing the same `/v1/fetch` contract ([`bench/aiseek`](../bench/aiseek)).

**Production logic** (plain HTTP first, render when the text is short):

| Metric | Crawl4AI | pagesift |
|---|---|---|
| Time per document, p50 ↓ | 1.81 s | **0.86 s** |
| Time per document, p95 ↓ | 9.47 s | **4.66 s** |
| Documents with content ↑ | 176 | **182** |
| Peak memory ↓ | 1,379 MB | **225 MB** |
| Average CPU ↓ | 0.82 cores | **0.45 cores** |

**Every URL rendered:**

| Metric | Crawl4AI | pagesift |
|---|---|---|
| Latency p50 / p95 / p99 ↓ | 1.97 / **5.90** / 20.96 s | **1.55** / 6.56 / **13.04** s |
| Successful with ≥ 200 chars of text ↑ | 169 | **181** |
| Reported a redirect's status instead of the final one ↓ | 132 | **0** |
| Service errors ↓ | 37 (incl. bare 500s) | **21** (all classified) |
| Peak memory ↓ | 1,402 MB | **680 MB** |

**Concurrency** (25 pages × ~10 rounds):

| Concurrent pages | Throughput, req/s ↑ | p50 ↓ | Peak memory ↓ |
|---|---|---|---|
| 4 | 2.10 → **2.82** | 3.20 → **2.37** s | 2,041 → **327** MB |
| 8 | 2.10 → **4.33** | 6.67 → **2.76** s | 2,052 → **542** MB |

**Cold start:** first render 1.93 s → **0.19 s**; ready to serve 1.27 s → **0.12 s**.

## 5. Deployment footprint

| | Crawl4AI | pagesift |
|---|---|---|
| Install | Python + 115 packages (~1 GB) + Playwright browsers (658 MB) | one 33 MB binary; Lightpanda (76 MB) for rendering |
| Docker image ↓ | 6.6 GB (AiSeek's, incl. its models) | TODO: measure the release image |
| Cold start ↓ | 1.3 s + 1.7 s browser launch | 0.12 s |
