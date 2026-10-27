<div align="center">

<img src="https://raw.githubusercontent.com/YOLOW92/pagesift/main/assets/logo.png" alt="PageSift Logo" width="300" />

# pagesift 0.80.1

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

## How PageSift compares

Measured on 2026-10-27 with the 0.80.0 release binary against three tools per domain (one Rust
project and two widely compared ones), default settings, on one machine (i7-10700, 15 GB). Samples
were drawn with fixed seeds after the rules were frozen; nothing was hand-picked. Every row links to
the method, versions, sample counts and raw results.

### Far ahead

| Domain | Metric (direction) | PageSift 0.80 | Strongest competitor | Evidence |
| :--- | :--- | :--- | :--- | :--- |
| Office & e-books (docx, xlsx, pptx, epub, csv) | Structure kept, held-out ↑ | **0.988** | MarkItDown 0.717 | [§4](benchmarks/mix/results/report-2026-10-27.md) |
| PDF, olmOCR-bench born-digital (1,269 pages) | Official score ↑ | **0.626** | pymupdf4llm 0.484 | [§3](benchmarks/mix/results/report-2026-10-27.md) |
| Hard web pages (166 adversarial) | Completion ↑ / timeouts ↓ | **0.723** / 0 | crw 0.590 / 27 | [§1](benchmarks/mix/results/report-2026-10-27.md) |
| Office & e-books speed (81 files, 1 worker) | Files/s ↑ | **57.4** | extractous 9.9 | [§4](benchmarks/mix/results/report-2026-10-27.md) |

### Ahead

| Domain | Metric (direction) | PageSift 0.80 | Strongest competitor | Evidence |
| :--- | :--- | :--- | :--- | :--- |
| Main-content extraction, WCXB | F1 ↑ | **0.866** | Readability 0.848 | [§1](benchmarks/mix/results/report-2026-10-27.md) |
| Main-content extraction, AEB | F1 ↑ | **0.956** | trafilatura 0.950 | [§1](benchmarks/mix/results/report-2026-10-27.md) |
| Main-content extraction speed (269 pages, 16 workers) | Pages/s ↑ | **614** | crw 387 | [§1](benchmarks/mix/results/report-2026-10-27.md) |
| PDF speed (144 files, 1 worker) | Files/s ↑ | **114** | pdf_oxide 94 | [§3](benchmarks/mix/results/report-2026-10-27.md) |

### Behind but close — actively being worked on

| Domain | Metric (direction) | PageSift 0.80 | Strongest competitor | Evidence |
| :--- | :--- | :--- | :--- | :--- |
| PDF vs a layout model (144-page subset) | olmOCR score ↑ | 0.650 | Docling (CPU) 0.696 | [§3](benchmarks/mix/results/report-2026-10-27.md) |
| Random real PDFs (71) | Text completeness ↑ | 0.936 | pdf_oxide 0.962 | [§3](benchmarks/mix/results/report-2026-10-27.md) |
| Fetching, 269 random pages (local replay) | Completion ↑ | 0.937 | Scrapy 0.981 | [§2 — pagesift refuses captcha/block pages](benchmarks/mix/results/report-2026-10-27.md) |
| Main-content extraction, Firecrawl set | Truth recall ↑ | 0.477 | crw 0.511 | [§1](benchmarks/mix/results/report-2026-10-27.md) |
| Hard PDFs (100 pdf.js test files) | Completion ↑ | 0.560 | pymupdf4llm 0.680 | [§3 — the largest gap](benchmarks/mix/results/report-2026-10-27.md) |

### Behind

| Domain | Metric (direction) | PageSift 0.80 | Strongest competitor | Evidence |
| :--- | :--- | :--- | :--- | :--- |
| Fetching speed (269 pages, 1 worker) | Pages/s ↑ | 2.7 | spider 153 | [§2 — default auto engine, p95 1.5 s](benchmarks/mix/results/report-2026-10-27.md) |
| PDF speed (144 files, 16 workers) | Files/s ↑ | 142 | pdf_oxide 526 | [§3 — one process per file scales poorly](benchmarks/mix/results/report-2026-10-27.md) |

Results for the other domains (dates, Markdown fidelity, RAG chunking, crawling at scale) — each with its direction, version and date — are in
[docs/benchmarks.md](docs/benchmarks.md); scripts and raw results per domain are under
[benchmarks/](benchmarks/).

---

## Scale: 200,000,000 requests on one machine (0.70)

- 🏁 **200M saturation on 192 vCPU (synthetic local origin, no network — not a real-internet speed)** — `200,000,000 / 200,000,000`, **0 failures**, **947,611 pages/s**, 211.1 s wall, **888.1 MiB peak RSS**. Source: [`0.70_200m_regression.md`](docs/history/0.70_200m_regression.md).
- 🧠 **Memory is an input.** The engine reads cgroup v2 `memory.max` and sizes itself to it: at **768 MiB** it settles on 380 connections and still sustains **817,346 pages/s** with zero OOM. Source: [0.70.0 release notes](docs/releases/0.70.0.md).

Measured with 0.70.0, not re-run for later versions.

---

## What's New in 0.80.1

Fixes from the 0.80 review (HTML extraction is untouched; PDF output changes only where a raised
footnote marker follows a word; speed within 1% of 0.80.0 on the same machine): CSV in Shift_JIS, GBK or
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
  -e PAGESIFT_API_KEYS=sk-test ghcr.io/yolow92/pagesift:0.80.1
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
  note   = {Version 0.80.1}
}
```

---

## ⭐ Star History

[![Star History Chart](https://api.star-history.com/svg?repos=YOLOW92/pagesift&type=Date)](https://star-history.com/#YOLOW92/pagesift&Date)
