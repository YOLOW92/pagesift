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

## 2. Publication dates

**Dataset:** the 1,000 human-annotated pages of [htmldate](https://github.com/adbar/htmldate)'s evaluation set: 800 multilingual pages (BBAW collection, mostly German) and 200 worldwide news pages (Data Culture Group, Northeastern University). Scoring is htmldate's own: the date is compared by day; no answer on a page that has a date is a miss, a wrong date is a false positive. No URL is given to any tool. Pages are split by `sha1(url) % 2` into a tuning half and a hold-out half, and pagesift changes were kept only if the hold-out half did not get worse.

**Metrics:** precision, recall and F1 (higher is better); time per page (lower is better). For pagesift, time is shown both for the date search alone and for the whole page conversion.

| Tool | F1 ↑ | F1, hold-out ↑ | Precision ↑ | Recall ↑ | ms per page ↓ |
|---|---|---|---|---|---|
| **pagesift 0.12** | 0.948 | **0.945** | **0.909** | 0.991 | **0.06** (date) / 0.71 (whole page) |
| htmldate 1.11 (extensive) | **0.949** | **0.945** | 0.908 | **0.993** | 4.04 |
| trafilatura (uses htmldate) | 0.948 | 0.944 | 0.907 | 0.992 | 9.79 |
| htmldate 1.11 (fast) | 0.926 | 0.924 | 0.924 | 0.929 | 2.00 |
| newspaper4k 0.9.6 | 0.810 | 0.802 | 0.912 | 0.728 | 11.74 |
| goose3 3.1.22 | 0.742 | 0.733 | 0.830 | 0.671 | 71.80 |
| pagesift before (meta, JSON-LD and `<time>` only, as Firecrawl and Crawl4AI read them) | 0.789 | 0.778 | 0.897 | 0.704 | — |

pagesift ports htmldate's search order and rules to Rust and adds month names in about twenty languages (htmldate needs the slow dateparser package for most languages), keeps the page's own time zone instead of converting to UTC, ignores "today's date" banners, and replaces the full-page regular expressions with single-pass scanners (`crates/pagesift-content/src/dates.rs`). The result matches htmldate's accuracy at 1/67 of its time.

Reproduce: `bench/meta/dates.py` (see [`bench/meta/README.md`](../bench/meta/README.md)); full results: [`bench/meta/results-dates-2026-10-21.md`](../bench/meta/results-dates-2026-10-21.md).

## 3. Crawling over HTTP (no rate limit)

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

## 4. Crawling messy real-world sites: coverage, waste and politeness

Speed on clean sites says little about what a crawl delivers on real sites, where links carry tracking parameters and session IDs, the same page has several URLs, calendars and pagination never end, and servers fail or rate-limit. This benchmark measures how much of a site's real content each tool collects within the same page budget, and how well it behaves.

**Setup:** a seeded simulation of the web ([`bench/crawl/src/bin/wild.rs`](../bench/crawl/src/bin/wild.rs)): 40 sites, each with 60 content pages (real corpus pages) linked so that following clean links alone reaches all of them. Each site gets a random subset of real-world problems: tracking parameters and session IDs on links, print versions and `index.html` aliases, relative links that grow forever (`more/more/…`), endless calendars and pagination, sort/filter combinations, links to images and archives, a robots.txt-disallowed area linked from every page, random first-attempt 503s, and a 5-requests-per-second limit answered with 429. Latency is 10–80 ms per site. Every tool may fetch 60 pages per site, so every wasted request costs a content page. The origin itself classifies every request, so no tool reports its own score.

**Metrics:**
- *coverage*: distinct content pages fetched ÷ 60 (higher is better);
- *waste*: share of requests that were duplicates, traps, binaries, robots-disallowed or 404 (lower is better);
- *robots violations*: requests to disallowed paths (lower is better);
- *429s*: rate-limit responses received (lower is better);
- *time* and *memory* (peak PSS of the whole process tree): lower is better.

Scenarios: *fast* (no interval, 4 requests per host in flight) and *polite* (one request per host every 200 ms).

| Seed | Scenario | Tool | Coverage ↑ | Waste ↓ | Robots violations ↓ | 429s ↓ | Time, s ↓ | Memory, MB ↓ |
|---|---|---|---|---|---|---|---|---|
| 2 | fast | **pagesift** | **0.747** | **0.116** | **0** | 81 | 21.4 | **79** |
| | | Crawl4AI 0.9 (HTTP) | 0.499 | 0.429 | 102 | **6** | 114.0 | 1,189 |
| | | crw | 0.367 | 0.403 | **0** | 236 | 14.9 | 494 |
| | | Spider | 0.358 | 0.367 | **0** | 271 | **1.4** | 391 |
| | | Scrapy | 0.332 | 0.555 | **0** | 297 | 18.4 | 1,034 |
| 2 | polite | **pagesift** | **0.736** | **0.120** | **0** | **0** | 42.3 | **78** |
| | | Crawl4AI 0.9 (HTTP) | 0.529 | 0.392 | 60 | **0** | 113.5 | 1,859 |
| | | Spider | 0.470 | 0.436 | **0** | **0** | **12.5** | 701 |
| | | crw | 0.367 | 0.403 | **0** | 236 | 15.0 | 419 |
| | | Scrapy | 0.197 | 0.725 | **0** | 48 | 14.6 | 1,351 |
| 1 | fast | **pagesift** | **0.770** | **0.115** | **0** | 122 | 19.7 | — |
| | | Crawl4AI 0.9 (HTTP) | 0.492 | 0.446 | **0** | **6** | 125.3 | — |
| | | crw | 0.324 | 0.371 | **0** | 339 | 15.5 | — |
| | | Spider | 0.314 | 0.373 | **0** | 335 | **1.4** | — |
| | | Scrapy | 0.319 | 0.435 | **0** | 1,049 | 17.2 | — |
| 1 | polite | **pagesift** | **0.755** | **0.122** | **0** | **0** | 17.6 | — |
| | | Crawl4AI 0.9 (HTTP) | 0.505 | 0.422 | **0** | **0** | 141.5 | — |
| | | Spider | 0.455 | 0.447 | **0** | **0** | **12.5** | — |
| | | crw | 0.324 | 0.371 | **0** | 339 | 15.4 | — |
| | | Scrapy | 0.185 | 0.727 | **0** | 40 | 15.4 | — |

What makes the difference (see `crates/pagesift/src/frontier.rs`): tracking parameters and session IDs are stripped before fetching; binary files and self-repeating paths are skipped; URL rewrite rules (`index.html`, `?print=1`, `more/`) are learned from duplicate pages; URL templates that keep returning duplicate content are moved behind normal pages; transient failures are retried later without blocking the crawl; and a host that answers 429 gets a wider request interval. Spider is fastest because it does not wait for rate limits or retry failures. pagesift's polite run with seed 2 is slower because one site answered its first four requests with 503 and pagesift backed off for about 30 seconds, as intended.

**Real sites, no regressions:** the same rules were checked on 40 sites drawn at random from the Tranco list (crawl limit 40 pages, HTTP engine, robots.txt respected), comparing the crawler before and after these changes ([`bench/crawl/real_sites.py`](../bench/crawl/real_sites.py)). Distinct pages delivered went from 1,277 to 1,312 (higher is better). 33 sites were identical, 4 improved (one went from 0 to 40 pages), and one lost a single distinct page. The two sites with more errors returned Cloudflare challenges to the second run, which pagesift reports and never bypasses.

Reproduce: `SEED=2 bench/crawl/run_wild.sh` (any seed draws a different web); full results: [`bench/crawl/results-wild-2026-10-21.md`](../bench/crawl/results-wild-2026-10-21.md).

## 5. Browser rendering

pagesift drives browsers over its own CDP client and picks an engine automatically: **Lightpanda** first, **moli** when a page needs iframes or more complete JavaScript, **Chrome / chrome-headless-shell** last. Same crawl setup as above, every page rendered.

| Engine | wide pages/s ↑ | huge pages/s ↑ | Peak memory ↓ |
|---|---|---|---|
| **pagesift auto (adaptive pool)** | **~80–95** | **~82–91** | 0.5–0.9 GB |
| Lightpanda only | 70.9 | 65.1 | ~0.48 GB |
| chrome-headless-shell | 33 | 34 | 1.4–1.5 GB |
| moli | 28.8 | 29.6 | 0.84–0.9 GB |
| Google Chrome | 11.6 | 11.7 | 2.2–2.4 GB |

Reproduce: `bench/crawl/engines.sh` (set `ENGINES=auto` for the default chain).

## 6. A production workload: replacing Crawl4AI

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

## 7. Deployment footprint

| | Crawl4AI | pagesift |
|---|---|---|
| Install | Python + 115 packages (~1 GB) + Playwright browsers (658 MB) | one 33 MB binary; Lightpanda (76 MB) for rendering |
| Docker image ↓ | 6.6 GB (AiSeek's, incl. its models) | TODO: measure the release image |
| Cold start ↓ | 1.3 s + 1.7 s browser launch | 0.12 s |
