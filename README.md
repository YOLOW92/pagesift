<div align="center">

<img src="https://raw.githubusercontent.com/YOLOW92/pagesift/main/assets/logo.png" alt="PageSift Logo" width="300" />

# pagesift 0.81.3

**High-performance, memory-bounded web scraping and extraction engine built for AI agents and LLM pipelines.**  
Turn any website, document, or archive into clean Markdown and structured data — one Rust binary, native SDKs, dual Firecrawl v1/v2 & Jina Reader APIs, at a fraction of the memory.

[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](LICENSE)
[![Commercial license](https://img.shields.io/badge/commercial-license%20available-green.svg)](COMMERCIAL.md)
[![Release](https://img.shields.io/github/v/release/YOLOW92/pagesift?include_prereleases)](https://github.com/YOLOW92/pagesift/releases)
[![crates.io](https://img.shields.io/crates/v/pagesift.svg)](https://crates.io/crates/pagesift)
[![PyPI](https://img.shields.io/pypi/v/pagesift.svg)](https://pypi.org/project/pagesift/)
[![npm](https://img.shields.io/npm/v/@pagesift/node.svg)](https://www.npmjs.com/package/@pagesift/node)
[![200M Saturation](https://img.shields.io/badge/200M%20Synthetic-947%2C611%20p%2Fs-blue.svg)](#scale-200000000-requests-on-one-machine-070)
[![768 MiB cgroup](https://img.shields.io/badge/768%20MiB%20cgroup-Zero%20OOM-success.svg)](#scale-200000000-requests-on-one-machine-070)
[![M8ven Score](https://m8ven.ai/badge/mcp/yolow92-pagesift-0ygyrw)](https://m8ven.ai/mcp/yolow92-pagesift-0ygyrw?s=readme)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/esA3wMZVvA)

**English** · [简体中文](docs/README.zh-CN.md) · [日本語](docs/README.ja.md) · [Español](docs/README.es.md)

</div>

---

<!-- perf-glance -->
## Performance at a glance (0.81)

PageSift 0.81 was re-measured on 2026-10-29; competitor numbers are the recorded 2026-10-21/22/27/28 data and were not re-run. One row per workload, best PageSift configuration; memory is a column, not a separate rank.


### Far ahead

| Domain | Workload | Metric | PageSift 0.81 | Fastest rival | Rival | Gap | Peak MB/worker (PageSift / rival) |
|---|---|---|---|---|---|---|---|
| PDF → Markdown | olmOCR-bench 1,269 pages, per page (single thread) | ms/page | **2.2** | pypdf | 50.0 | **×22.32** | — |
| HTML → Markdown | 600 generated documents, per page | ms/page | **0.24** | Turndown | 1.9 | **×8.24** | — |
| Main content | Firecrawl 597 pages, single thread | p50 ms/page | **1.0** | crw | 7.4 | **×7.23** | — |
| Publication dates | htmldate 1,000 pages, date search | ms/page | **0.31** | htmldate (fast) | 2.0 | **×6.45** | — |
| Office / e-books | B4 81 files, in-process | files/s | **94.1** | Kreuzberg | 19.5 | **×4.82** | 38 / 231 |
| RAG chunking | 5 corpora, 1.4 MB | total ms | **11** | chonkie recursive | 35 | **×3.24** | — |
| PDF → Markdown | B1 144 pages, 1 worker | files/s | **258.5** | pdf_oxide | 100.6 | **×2.57** | 163 / 29 |
| RAG query filter | 472 questions, default settings | answer recall | **0.929** | Crawl4AI BM25 | 0.673 | **+0.256** | — |
| Wild crawl | polite, 40 sites × 60 pages, coverage | coverage | **0.740** | Crawl4AI (HTTP) | 0.529 | **+0.211** | — |
| Wild crawl | fast, 40 sites × 60 pages, coverage | coverage | **0.685** | Crawl4AI (HTTP) | 0.499 | **+0.186** | — |
| Office / e-books | B4 hold-out 39 files, structure kept | structure kept | **0.988** | Kreuzberg | 0.816 | **+0.172** | — |
| PDF → Markdown | olmOCR-bench 1,269 pages | official score | **0.628** | pymupdf4llm | 0.484 | **+0.144** | — |


### Ahead

| Domain | Workload | Metric | PageSift 0.81 | Fastest rival | Rival | Gap | Peak MB/worker (PageSift / rival) |
|---|---|---|---|---|---|---|---|
| PDF → Markdown | B1 144 pages, 16 workers | files/s | **580.6** | pdf_oxide | 501.7 | **×1.16** | 14 / 11 |
| Main content | A2 269 random pages, 16 workers | pages/s | **630.0** | Resiliparse | 559.3 | **×1.13** | 17 / 54 |
| HTML → Markdown | 600 generated documents, fidelity | mean F1 | **1.000** | markdownify | 0.936 | **+0.064** | — |
| Main content | WCXB test split, 150 pages | F1 | **0.866** | Readability + Turndown | 0.848 | **+0.018** | — |


*A2 was re-fetched on 2026-10-29; its absolute times are not directly comparable with the 10-28 A2.*


[Full report](benchmarks/history/0.81/results/20261029-102426/report.md)

<!-- /perf-glance -->

---

## How PageSift compares

**Still behind:** A2 replay, 269 pages, 1 worker 3.1 pages/s in the 0.81.2 local replay (2.4 in the 0.81 report) vs spider 156.6 pages/s; fast, 40 sites × 60 pages, time 16.6 seconds vs spider 1.4 seconds; B5 134 pages, completion 0.112 vs Docling (GPU) 1.000.

*A2 throughput is measured against a replay server on a single loopback address, so all 269 pages count as one host. PageSift waits 1 s between requests to the same host by default; spider has no such limit. This row mostly reflects that politeness interval, not engine speed.*

[Full report](benchmarks/history/0.81/results/20261029-102426/report.md).

---

## Scale: 200,000,000 requests on one machine (0.70)

- 🏁 **200M saturation on 192 vCPU (synthetic local origin, no network — not a real-internet speed)** — `200,000,000 / 200,000,000`, **0 failures**, **947,611 pages/s**, 211.1 s wall, **888.1 MiB peak RSS**. Source: [`0.70_200m_regression.md`](docs/history/0.70_200m_regression.md).
- 🧠 **Memory is an input.** The engine reads cgroup v2 `memory.max` and sizes itself to it: at **768 MiB** it settles on 380 connections and still sustains **817,346 pages/s** with zero OOM. Source: [0.70.0 release notes](docs/releases/0.70.0.md).

Measured with 0.70.0, not re-run for later versions.

---

## What's New in 0.81.3

**Tag filters and authenticated Jina routes.** Firecrawl's `includeTags` / `excludeTags` now work:
CSS selectors (tag, `.class`, `#id`, …) keep or drop elements before extraction, `<head>` metadata
is kept and `rawHtml` is unchanged; an invalid selector is rejected before fetching. The Jina Reader
routes (`/r/`) now require an API key when keys are configured and count against rate limits and
page quotas. Details: [0.81.3 release notes](docs/releases/0.81.3.md).

## What's New in 0.81.2

**Zero content loss for adaptive upgrades.** A bare client-side redirect with no HTTP content now
captures its target's DOM as soon as main-frame text appears, instead of waiting for
`DOMContentLoaded` or network idle. The wait is bounded relative to the page's HTTP time and the
caller's timeout. On the 269-page A2 replay, 0.81.0's 110.7 s / 21.9 s (1/16 workers) becomes
88.0 s / 13.5 s, with **zero pages losing content** and 174 more extracted characters. Details:
[0.81.2 release notes](docs/releases/0.81.2.md).

## What's New in 0.81.1

**Adaptive browser upgrades.** `engine: auto` now stops a browser upgrade when the extracted main
content stops improving, using online distributions of navigation and settle times instead of fixed
seconds. On the 269-page A2 replay this cuts 16-worker wall time from 28.4 s to 10.5 s (9.5 → 25.7
pages/s) and 1-worker time from 119.2 s to 77.3 s, with 301 fewer extracted characters over the
whole set. Pages whose HTTP extraction was empty still get the full render time. Details:
[0.81.1 release notes](docs/releases/0.81.1.md).

## What's New in 0.81.0

**Relevance by facet, and faster everywhere.** Focused crawls score how much a page is *about* each
facet of the query (query words, plus adjacent query words the site uses as a phrase, such as
*computer science*; title, URL and headings count more than the body; each facet weighted by its
rarity on the site), all estimated from the crawl's own pages. On 1,833 pages judged by content,
separating useful from useless pages improves from AUC 0.79 to 0.86 on held-out universities.
Speed: an allocator cache per thread and NUMA node, inline glyph strings for PDF, a linear title
check for HTML, contiguous tables for CSV and xlsx, fat LTO and profile-guided release builds; output
is byte-identical to 0.80.10. Details: [0.81.0 release notes](docs/releases/0.81.0.md).

## What's New in 0.80.10

**An adaptive page limit.** `crawl --query … --adaptive` no longer needs `--limit`: the crawl keeps
going while pages add new relevant content and stops when they stop doing so, or when nothing
relevant can be found (`no_relevant_pages`); the limit is only a cap (500 when not given). On 69
universities in five directions the adaptive crawl found 4,208 pages squarely on the query
against 645 with `--limit 12` and 1,856 with `--limit 100` (0.80.9), and a larger share of
its pages was useful (25.7% against 20.0% and 17.2%). Discovery: foreign-language main
sites no longer take the budget of an English query, and `/about/housing/` is content, not an
"about" page. Extraction: press-release teasers, news sidebars and relative links in uploads
(`/v2/parse` takes a `url` field). Details: [0.80.10 release notes](docs/releases/0.80.10.md).

## What's New in 0.80.9

**The last 0.80 release.** Focused crawling was checked on 69 universities in five directions
(international, graduate admission, housing, computer science, library): pages squarely on the query
2.6% → 19.0%, on or near it 18.4% → 49.6% against 0.80.0; queued links are now re-scored as term
weights are learned, and one page reached under several URLs is delivered once. PDFs include the
text of filled-in form fields, notes, stamps and signatures (hard pdf.js test files: 53 → 61 of 100).
`scrape` starts lighter when run once per file (CPU per call 6.3 → 4.3 ms). `doctor` starts every
engine and warns when one cannot run; engines outside `PATH` and in Playwright's cache are found;
`crawl`/`map` take `--ignore-robots`. Accordion and FAQ headers stay in extracted content. Every
benchmark stays within 2% of 0.80.0. There were no 0.80.6–0.80.8. Details:
[0.80.9 release notes](docs/releases/0.80.9.md).

## What's New in 0.80.5

**Results that look fine but are not now say so.** Warnings that mean a result is incomplete or
wrong (lost PDF pages or content, thin or partial text, truncated output, a shell render …) now fill
Firecrawl's `data.warning`, so Firecrawl clients see them; `crawl --out` adds `summary.json`. Age
gates inside a big site frame (Steam's `/agecheck/`) and HTML block pages labelled as PDF are
reported as `ACCESS_BARRIER` instead of content. Word, Excel, PowerPoint, CSV and EPUB stop at 16 MB
of text with `output_truncated` (a 408 KB DOCX used to expand into 84 MB of Markdown). PDF damage
warnings count against the document's real page count and name half-lost pages. Rendering waits up
to 1.5 s more for pages that are still an empty shell when the network goes quiet. Misspelled
`--engine`/`--format`/`--sitemap` values are errors. Every benchmark stays within 2% of 0.80.0.
There was no 0.80.4. Details: [0.80.5 release notes](docs/releases/0.80.5.md).

## What's New in 0.80.3

**Focused crawling, checked on real university sites.** The same 38 university home pages were
crawled by 0.80.0 and 0.80.3 with `crawl --query "international students admission tuition
scholarship visa" --adaptive --limit 12`. On the 32 sites both reached: useful pages 29.6% →
66.1%, international-student pages 6.3% → 38.9%, median useful pages per site 1.5 → 8 (graded
automatically by URL and title; scripts and raw results in `benchmarks/history/0.80.3/`). Query
crawls now cover the whole site, refund failed fetches to the page budget, find the English site
through `hreflang` and English subdomains, and stop treating menus as lists. A PDF that lost a
page's content to truncation now says which page (`content_missing`). Details:
[0.80.3 release notes](docs/releases/0.80.3.md).

## What's New in 0.80.2

**The focused crawl finds the pages it was asked for.** A tester ran `crawl --query` over 159
university sites (*international students admission tuition scholarship visa*, 12 pages each) and
got 25.6% useful pages. The link scoring, scheduling and stopping rule are rewritten: anchor text
for every link (navigation included), query terms weighted by how rare they are on the site, news
and about pages demoted, sections taken in turn, subdomains in scope, error pages excluded, and an
adaptive stop that only counts relevant pages. On the synthetic 8-site gate that reproduces the
report's failure patterns, useful pages went from 7.3% to 82.6%, and two synthetic sites from other
domains (a developer portal, a city services site) with their own queries keep the rules honest;
**real sites were not reachable from the development container, so those numbers are synthetic
only.** New: `attachments` fetches
the PDFs and Office documents linked from relevant pages; every result carries a crawl trace
(`depth`, `parent`, `score`, `attachmentOf`). PDF follow-ups: a title under section-only bookmarks
is no longer empty; a linearized PDF cut before its page tree root is converted instead of
rejected. Details: [0.80.2 release notes](docs/releases/0.80.2.md).

## What's New in 0.80.1

Fixes from the 0.80 review (HTML extraction is untouched; PDF output changes only where a raised
footnote marker follows a word): CSV in Shift_JIS, GBK or
any declared charset is decoded like HTML (it was mojibake with no warning); a damaged PDF that is
only partly recovered says so (`pdf_repaired`, `pages_missing`); footnote markers after author
names are no longer written as formulas (`Ashish $Vaswani^{\ast}$` → `Ashish Vaswani*`); a PDF
without an embedded title gets the inferred one in `metadata.title` (`titleSource: inferred`);
`*.localhost` resolves to loopback on every machine, so the test suite and performance gates run on
IPv4-only hosts; `NOTICE` (now with the Adobe cmap-resources license) ships with every binary.
Details: [0.80.1 release notes](docs/releases/0.80.1.md).

## What's New in 0.80.0

**PDFs that keep their structure, results that admit what they missed.**

- **Own PDF file layer** replaces lopdf: xref, object streams, filters, encryption and repair of
  broken files, faster and with less memory.
- **PDF headings with real levels**, read from the writer's structure tree and bookmarks first and
  inferred from numbering, size and weight otherwise. On the 13 PDFs of the AiSeek report
  (higher is better): recall 0.595 → 0.947, precision 0.400 → 0.876, level agreement 0.819 →
  0.957, empty headings 3 → 0.
- **Headers, footers and figures**: `/Artifact` marks, running heads with page numbers and vector
  figures are recognised. olmOCR-bench `headers_footers` 0.681 → 0.734 (higher is better); false
  tables on the 13 PDFs 196 → 9 (lower is better).
- **CJK text without ToUnicode**: Chinese, Japanese and Korean PDFs whose CID fonts carry no
  `/ToUnicode` map (LaTeX/ctex with Fandol fonts, many others) lost all CJK text; it is now decoded
  through the Adobe-GB1/CNS1/Japan1/Korea1 collections or the embedded TrueType cmap. On 19 diverse
  Chinese PDFs, inferred-heading recall 0.497 → 0.826 (higher is better).
- **`POST /v2/parse`**: Firecrawl-compatible upload of PDF, HTML, Word (.docx), Excel (.xlsx), PowerPoint (.pptx), EPUB and CSV files; `/v2/scrape` and `pagesift scrape <file>` convert the same formats.
- **`partial_content` and `replies_omitted` warnings** when the extracted text is likely
  incomplete. On 1,842 saved WCXB pages, losses without a warning 0.842 → 0.667 and false alarms
  0.174 → 0.130 (both lower is better).
- **Fixes**: fast pages in a batch no longer wait for slow pages on the same host; MCP audit
  records name older-protocol clients; `pagesift doctor` leaves the audit log alone.

Full detail, sources and the held-out results: [**0.80.0 release notes**](docs/releases/0.80.0.md).

Earlier releases (0.73, 0.72.8, 0.71, 0.70, …): [CHANGELOG](CHANGELOG.md) and the
[release notes](docs/releases/). Retired homepage sections — the 0.70 seven-domain table, the 0.60
100M-request run and the mixed-vintage comparison table — are kept unchanged, with their sources, in
[docs/history/homepage-en.md](docs/history/homepage-en.md).

---

## Quick Start

### 1. Docker / Self-Hosted Server

Run the standalone container (includes headless browser renderer):

```bash
docker run -d -p 3002:3002 -v pagesift-data:/data \
  -e PAGESIFT_API_KEYS=sk-test ghcr.io/yolow92/pagesift:0.81.3
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

#### Upload a File (`/v2/parse`)
Parse a local or non-public PDF, HTML, Word, Excel, PowerPoint, EPUB or CSV file without hosting it at a URL — Firecrawl-compatible multipart upload, up to 50 MB, same response as `/v2/scrape` ([details](docs/parse-api.md)). Office, EPUB and CSV conversion is pagesift's own Rust code (its own ZIP and XML readers); on the command line, `pagesift scrape report.docx` does the same:

```bash
curl -s http://localhost:3002/v2/parse -H 'Authorization: Bearer sk-test' \
  -F file=@report.pdf -F 'options={"formats":["markdown"]}'
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
  note   = {Version 0.81.3}
}
```

---

## ⭐ Star History

[![Star History Chart](https://api.star-history.com/svg?repos=YOLOW92/pagesift&type=Date)](https://star-history.com/#YOLOW92/pagesift&Date)
