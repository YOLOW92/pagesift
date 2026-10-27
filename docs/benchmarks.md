# Benchmarks

## 0.80 head-to-head (mixed benchmark, 2026-10-27)

The 0.80.0 release binary against three tools per domain — one Rust project and two widely compared
ones — with default settings, on one machine (i7-10700, 15 GB RAM, GTX 1070). Rules, fixed seeds and
sample lists were frozen before sampling; every sampled item counts, and an item a tool did not finish
scores 0. Full tables (with versions, sample counts, directions and speed) and raw per-item results:
[benchmarks/mix/results/report-2026-10-27.md](../benchmarks/mix/results/report-2026-10-27.md);
rules: [benchmarks/mix/RULES.md](../benchmarks/mix/RULES.md); tool versions and install commands:
[benchmarks/mix/tools.lock.md](../benchmarks/mix/tools.lock.md).

| Domain | Metric (direction) | pagesift 0.80.0 | crw / spider / pdf_oxide / extractous (Rust) | Two widely compared tools |
| :--- | :--- | ---: | ---: | :--- |
| Main content, AEB | F1 ↑ | **0.956** | crw 0.846 | trafilatura 0.950, Readability 0.937 |
| Main content, WCXB | F1 ↑ | **0.866** | crw 0.815 | trafilatura 0.839, Readability 0.848 |
| Main content, WebMainBench | F1 ↑ | 0.833 | crw 0.761 | trafilatura 0.833, Readability **0.840** |
| Main content, Firecrawl set | truth recall ↑ | 0.477 | crw **0.511** | trafilatura 0.392, Readability 0.403 |
| Hard web pages (166) | completion ↑ | **0.723** | crw 0.590 | trafilatura 0.470, Readability 0.488 |
| Fetching, 269 random pages (replay) | completion ↑ | 0.937 | spider 0.970 | Scrapy **0.981**, Crawl4AI 0.926 |
| PDF, olmOCR-bench (1,269 pages) | official score ↑ | **0.626** | pdf_oxide 0.389 | pymupdf4llm 0.484 |
| PDF, olmOCR 144-page subset | official score ↑ | 0.650 | pdf_oxide 0.422 | pymupdf4llm 0.479, Docling (model) **0.696** |
| Random PDFs (71) | text completeness ↑ | 0.936 | pdf_oxide **0.962** | pymupdf4llm 0.931 |
| Hard PDFs (100) | completion ↑ | 0.560 | pdf_oxide 0.660 | pymupdf4llm **0.680** |
| Office & e-books, held-out (39) | structure kept ↑ | **0.988** | extractous 0.075 | MarkItDown 0.717 |
| Main content speed, 269 pages, 16 workers | pages/s ↑ | **614** | crw 387 | trafilatura 90, Readability 12 |
| Fetching speed, 269 pages, 1 worker | pages/s ↑ | 2.7 | spider **153** | Scrapy 82, Crawl4AI 0.6 |
| PDF speed, 144 files, 1 worker | files/s ↑ | **114** | pdf_oxide 94 | pymupdf4llm 3.9, Docling (CPU) 0.2 |
| PDF speed, 144 files, 16 workers | files/s ↑ | 142 | pdf_oxide **526** | pymupdf4llm 5.9, Docling (CPU) 0.3 |
| Office speed, 81 files, 1 worker | files/s ↑ | **57.4** | extractous 9.9 | MarkItDown 1.6 |

Older per-domain sections follow; each says which version and date it was measured with.

| Domain | Section | Scripts and results |
| :--- | :--- | :--- |
| Main-content extraction | [§1](#1-main-content-extraction-quality-and-speed) | [benchmarks/web-extraction/](../benchmarks/web-extraction/README.md) |
| Publication dates | [§2](#2-publication-dates) | [benchmarks/dates/](../benchmarks/dates/README.md) |
| HTML → Markdown fidelity | [§3](#3-html--markdown-fidelity) | [benchmarks/markdown/](../benchmarks/markdown/README.md) |
| Chunking and query filtering for RAG | [§4](#4-chunking-and-query-filtering-for-rag) | [benchmarks/rag/](../benchmarks/rag/README.md) |
| PDF → Markdown | [§5](#5-pdf-to-markdown) | [benchmarks/pdf/](../benchmarks/pdf/README.md) |
| Crawling and browser rendering | [§6](#6-crawling-over-http-no-rate-limit), [§7](#7-crawling-messy-real-world-sites-coverage-waste-and-politeness), [§8](#8-browser-rendering) | [benchmarks/crawl/](../benchmarks/crawl/README.md) |
| Production workload, deployment | [§9](#9-a-production-workload-replacing-crawl4ai), [§10](#10-deployment-footprint) | [benchmarks/production/](../benchmarks/production/README.md) |
| Office documents, EPUB, CSV | [§11](#11-office-documents-epub-and-csv-to-markdown) | [benchmarks/office/](../benchmarks/office/README.md) |
| Scale (200M requests, core scaling) | [200M report](history/0.70_200m_regression.md) | [benchmarks/scale/](../benchmarks/scale/README.md) |
| 0.70 and earlier | [history/](history/README.md) | [benchmarks/history/](../benchmarks/history/README.md) |

For the newer six-project, 545-page comparisons and explicit remaining gaps, see the [0.20.0 evidence index](../benchmarks/history/0.70/six/evidence/README.md). The older metrics below use different datasets and methods.

Every number here comes from scripts in [`benchmarks/`](../benchmarks) and can be rerun. Each metric says which direction is better. Tools see exactly the same inputs; timing runs use one tool at a time on an otherwise idle machine.

**Machine:** Intel Core i7-10700 (8 cores / 16 threads), 15 GB RAM, NVMe SSD, Ubuntu (Linux 7.0). **Date:** 2026-10-21 (development runs), 2026-10-22 (0.20.0 run), 2026-10-23 (0.30.0 release run). **pagesift:** the tables in sections 1–8 were measured with pagesift 0.12.0 (the version named in each table; sections 3–5 and 7 re-checked 2026-10-22); nothing in this file has been re-run for 0.73 or later. Sections 1–8 are historical data. (An automatic version replacement once turned `2.0` into `0.20.0` in three cells of this file; they are restored from the commit before it, 0f978efd.)

> **Release run (0.30.0, 2026-10-23):** expanded benchmark suite covering 8,854 random real pages, 120 wild sites, 148 non-UTF-8 encodings, 20,000-page (`wide`) and 50,000-page (`huge`) crawls, and 8 new competitors (`htmd`, `html-to-markdown-rs`, `go-readability`, `go-html-to-markdown`, `colly`, `mdream`, `jina-reader`, `crawlee-python`) under a strict **12 GiB memory ceiling** (`MemoryMax=12G`). Full comprehensive report: [`benchmarks/common/fresh/results-0.30.0.md`](../benchmarks/common/fresh/results-0.30.0.md). Earlier runs: [`benchmarks/common/fresh/results-2026-10-22.md`](../benchmarks/common/fresh/results-2026-10-22.md).

## 1. Main-content extraction: quality and speed

**Dataset:** the 597 HTML pages of Firecrawl's `scrape-content-dataset-v1` that have both an HTML snapshot and labelled truth/noise phrases (news, blogs, docs, shops, forums; many languages). Pages whose HTTP extraction is thin use a browser-rendered snapshot instead (92 pages), for every tool. Inputs are frozen in `benchmarks/data/inputs`; adapters are in [`benchmarks/web-extraction/compare`](../benchmarks/web-extraction/compare).

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

Reproduce: see [`benchmarks/web-extraction/compare/README.md`](../benchmarks/web-extraction/compare/README.md).

> **Audit note (2026-10-27):** the scores in this table (e.g. 0.733) could not be matched to a result file in the repository; the later runs in [`benchmarks/common/fresh/results-0.30.0.md`](../benchmarks/common/fresh/results-0.30.0.md) use different data and scorers. Treat the table as historical data for 0.12.0, not re-run.

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

Reproduce: `benchmarks/dates/dates.py` (see [`benchmarks/dates/README.md`](../benchmarks/dates/README.md)); full results: [`benchmarks/dates/results-dates-2026-10-21.md`](../benchmarks/dates/results-dates-2026-10-21.md).

## 3. HTML → Markdown fidelity

**Setup:** there is no standard dataset for conversion fidelity, so [`benchmarks/markdown/fidelity.py`](../benchmarks/markdown/fidelity.py) generates one: 600 random documents (3 seeds × 200) with headings, emphasis, strikethrough, inline code, links, images, line breaks, nested lists, tables, fenced code, quotes, rules, formulas, and text with Markdown characters that must stay literal (`*not bold*`, `1. not a list`, `<tag>`, `x | y`). They are rendered like real sites (wrapper `div`s and `span`s, syntax-highlighted code, KaTeX, MathJax, raw TeX awaiting MathJax). Each tool converts the whole page; the Markdown is parsed back with the same GFM parser and compared with the original structure, category by category. A reference conversion written straight from the structure scores 1.000 in every category, which calibrates the scorer.

**Metric:** F1 per category (higher is better), mean over the 15 categories; conversion time per page (lower is better).

| Tool | Mean F1 ↑ | Tables ↑ | Code blocks ↑ | Lists ↑ | Formulas ↑ | Line breaks ↑ | Literal text ↑ | ms per page ↓ |
|---|---|---|---|---|---|---|---|---|
| **pagesift 0.12** | **1.000** | **1.000** | **1.000** | **1.000** | **1.000** | **1.000** | **1.000** | **0.22** |
| markdownify 1.2.3 | 0.936 | 0.828 | **1.000** | 0.980 | 0.339 | **1.000** | 0.987 | 5.4 |
| Defuddle | 0.912 | **1.000** | **1.000** | 0.535 | 0.572 | **1.000** | 0.988 | 132 |
| html2text | 0.852 | 0.827 | 0.114 | 0.947 | 0.245 | **1.000** | 0.982 | 2.1 |
| MarkItDown 0.1.8 | 0.812 | 0.828 | 0.137 | 0.980 | 0.339 | 0.000 | 0.989 | 9.1 |
| Turndown (no GFM plugin) | 0.799 | 0.000 | **1.000** | 0.980 | 0.168 | **1.000** | 0.991 | 1.95 |
| Crawl4AI 0.9.5 | 0.787 | 0.622 | 0.132 | 0.845 | 0.245 | 0.615 | 0.970 | 2.0 |

"Literal text" is the word-level F1 of the text; it drops when unescaped `*`, `_`, `<` or line-start `1.` turn text into formatting. pagesift escapes only what CommonMark would actually misread (paired `*` and `_` next to text, but not `a_b_c` or `2 * 3`; line-start list and heading markers, but not inside table cells), turns `<br>` into hard breaks, and converts formulas to TeX: KaTeX and Wikipedia annotations, MathJax 2 scripts, MathML (MathJax 3, LaTeXML) through its own MathML→LaTeX converter, and raw TeX in the text left untouched.

On real pages the change is neutral for main-content scores (Firecrawl set 0.696 → 0.696, AEB 0.939 → 0.939); on 3,369 random sites it adds 0.13 backslashes per 1,000 characters, and on an arXiv page formulas that used to vanish now come out as TeX. Full results: [`benchmarks/markdown/results-2026-10-21.md`](../benchmarks/markdown/results-2026-10-21.md); reproduce: `benchmarks/markdown/run.sh`.

## 4. Chunking and query filtering for RAG

**Dataset:** Chroma's [chunking_evaluation](https://github.com/brandonstarxel/chunking_evaluation) data: five corpora (financial filings, PubMed papers, Wikipedia text, a State of the Union address, chat logs) and 472 questions, each labelled with the exact character spans of its answer. Questions are split into a tuning half and a hold-out half; design choices were kept only if the hold-out half agreed. Method details: [`benchmarks/rag/README.md`](../benchmarks/rag/README.md).

**Query filter.** Each question is asked against its own document (49,000 words on average); every tool runs with its default settings. Recall is the share of the answer text kept (higher is better); words is how much text is returned (lower is better).

| Tool | Answer recall ↑ | Words returned ↓ | Precision ↑ | ms per question ↓ |
|---|---|---|---|---|
| **pagesift 0.12 `queryFilter`** | **0.929** | **1,157** | **0.038** | **10.6** |
| Crawl4AI 0.9.5 `BM25ContentFilter` | 0.673 | 13,507 | 0.009 | 775 |
| pagesift before (paragraphs, absolute threshold 1.0) | **0.984** | 27,513 | 0.007 | — |

The old filter, like Crawl4AI's, worked on paragraphs with an absolute BM25 threshold of 1.0: on long documents almost everything passes, and a paragraph can be thousands of words long. pagesift now scores ~100-token units inside each section together with their heading path and keeps the units scoring at least half of the best one, then merges adjacent units back into the original text. At equal word budgets, these units retrieve twice as much of the answer as paragraphs (recall 0.715 vs 0.353 at 250 words).

**Chunking.** Every chunker cuts the corpora into chunks of about 200 words; one BM25 retriever (`bm25s`) searches all chunks, and chunks are taken in score order up to a word budget.

| Chunker | Recall @ 250 words ↑ | @ 500 ↑ | @ 1,000 ↑ | Time for 1.4 MB ↓ |
|---|---|---|---|---|
| **pagesift `recursive`** | 0.607 | **0.765** | **0.895** | **21 ms** |
| semchunk 4.1 | 0.607 | **0.765** | **0.895** | 72 ms |
| LangChain `RecursiveCharacterTextSplitter` | **0.626** | 0.762 | 0.893 | 135 ms |
| chonkie 1.7 `RecursiveChunker` | 0.587 | 0.755 | 0.893 | 35 ms |
| chonkie `SentenceChunker` | 0.553 | 0.722 | 0.878 | 118 ms |
| Crawl4AI `SlidingWindowChunking` | 0.561 | 0.708 | 0.827 | 146 ms |
| Crawl4AI `FixedLengthWordChunking` | 0.479 | 0.618 | 0.737 | 79 ms |

pagesift's `recursive` strategy is a Rust port of semchunk (identical chunks on this data) with Markdown awareness (headings stay with their content, code blocks are not split while they fit) and CJK punctuation. Three in-house variants (TextTiling-style cuts at lexical topic shifts, cohesion-based and balanced merging) did better on the tuning half but not on the hold-out half, so they were not kept. The chunks are available from the API with `"chunking": {"type": "recursive", "size": 400}`, each with its heading path and position.

Reproduce: `benchmarks/rag/run.sh`; full results: [`benchmarks/rag/results-2026-10-21.md`](../benchmarks/rag/results-2026-10-21.md).

> **Audit note (2026-10-27):** both tables match their results files. The query-filter row comes from the full re-run of 2026-10-22 (`benchmarks/common/fresh/results/2026-10-22/rag/query_filter.txt`, `pagesift-default`: 0.929 / 1,157 words / 0.038, with `minTokens` defaulting to 1000); the older `benchmarks/rag/results-2026-10-21.md` (0.874 / 622 / 0.137) predates that default.

## 5. PDF to Markdown

**Dataset:** [olmOCR-bench](https://huggingface.co/datasets/allenai/olmOCR-bench) (ODC-BY), scored with its own code. Only the five subsets made of born-digital PDFs are used (1,269 pages, about 6,000 tests): formulas from arXiv papers, tables, multi-column reading order, small dense text, and headers and footers. Each test checks one fact on one page — a sentence is present, a header is absent, two sentences are in order, a table cell has the right neighbours, a formula renders like the reference in KaTeX. Every tool runs with its default settings. Method details: [`benchmarks/pdf/README.md`](../benchmarks/pdf/README.md).

| Tool | Overall ↑ | Formulas ↑ | Tables ↑ | Reading order ↑ | Small text ↑ | Headers/footers removed ↑ | ms per page ↓ |
|---|---|---|---|---|---|---|---|
| **pagesift 0.12** | **71.0** | **44.3** | 56.1 | 61.2 | 35.7 | **68.0** | **3.4** |
| pymupdf4llm 1.28.2 (with its layout model) | 64.4 | 0.0 | **60.3** | **66.6** | 33.7 | 37.2 | 320 |
| pypdf 6.21.1 | 58.2 | 0.0 | 0.0 | 49.4 | **44.8** | 40.8 | 50 |
| MarkItDown 0.1.8 | 57.3 | 0.0 | 19.8 | 39.3 | 31.2 | 38.4 | 200 |
| pdfplumber 0.11.10 | 50.4 | 0.0 | 0.0 | 10.0 | 7.7 | 41.8 | 124 |

Scores are pass rates in percent. pagesift writes inline formulas as LaTeX: it reads the built-in encodings of the embedded TeX fonts, then rebuilds sub- and superscripts, accents and symbol commands from the glyphs' fonts, sizes and baselines. None of the other tools outputs formulas. Tables are found from ruled grids (pdfplumber's lattice algorithm, ported to Rust), from booktabs-style horizontal rules, and from the alignment of short text fragments when there are no rules at all. pymupdf4llm finds a few more tables and gets the reading order of more pages right with an ONNX layout model, at about 100× the time per page. Pages were split into a tuning half and a hold-out half, and changes were kept only when the hold-out half did not get worse.

Reproduce: `benchmarks/pdf/run.sh`; full results: [`benchmarks/pdf/results-2026-10-22.md`](../benchmarks/pdf/results-2026-10-22.md).

## 6. Crawling over HTTP (no rate limit)

**Setup:** a local multi-site origin ([`benchmarks/crawl`](../benchmarks/crawl)) serving real pages from the corpus on `s{i}.localhost`, with 10 ms of added latency; every tool crawls every site with its own link discovery, unthrottled. Memory is the peak proportional set size (PSS) of the tool's whole process tree.

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

Reproduce: `benchmarks/crawl/run_all.sh`.

## 7. Crawling messy real-world sites: coverage, waste and politeness

Speed on clean sites says little about what a crawl delivers on real sites, where links carry tracking parameters and session IDs, the same page has several URLs, calendars and pagination never end, and servers fail or rate-limit. This benchmark measures how much of a site's real content each tool collects within the same page budget, and how well it behaves.

**Setup:** a seeded simulation of the web ([`benchmarks/crawl/src/bin/wild.rs`](../benchmarks/crawl/src/bin/wild.rs)): 40 sites, each with 60 content pages (real corpus pages) linked so that following clean links alone reaches all of them. Each site gets a random subset of real-world problems: tracking parameters and session IDs on links, print versions and `index.html` aliases, relative links that grow forever (`more/more/…`), endless calendars and pagination, sort/filter combinations, links to images and archives, a robots.txt-disallowed area linked from every page, random first-attempt 503s, and a 5-requests-per-second limit answered with 429. Latency is 10–80 ms per site. Every tool may fetch 60 pages per site, so every wasted request costs a content page. The origin itself classifies every request, so no tool reports its own score.

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

**Real sites, no regressions:** the same rules were checked on 40 sites drawn at random from the Tranco list (crawl limit 40 pages, HTTP engine, robots.txt respected), comparing the crawler before and after these changes ([`benchmarks/crawl/real_sites.py`](../benchmarks/crawl/real_sites.py)). Distinct pages delivered went from 1,277 to 1,312 (higher is better). 33 sites were identical, 4 improved (one went from 0 to 40 pages), and one lost a single distinct page. The two sites with more errors returned Cloudflare challenges to the second run, which pagesift reports and never bypasses.

Reproduce: `SEED=2 benchmarks/crawl/run_wild.sh` (any seed draws a different web); full results: [`benchmarks/crawl/results-wild-2026-10-21.md`](../benchmarks/crawl/results-wild-2026-10-21.md).

## 8. Browser rendering

pagesift drives browsers over its own CDP client and picks an engine automatically: **Lightpanda** first, **moli** when a page needs iframes or more complete JavaScript, **Chrome / chrome-headless-shell** last. Same crawl setup as above, every page rendered.

| Engine | wide pages/s ↑ | huge pages/s ↑ | Peak memory ↓ |
|---|---|---|---|
| **pagesift auto (adaptive pool)** | **~80–95** | **~82–91** | 0.5–0.9 GB |
| Lightpanda only | 70.9 | 65.1 | ~0.48 GB |
| chrome-headless-shell | 33 | 34 | 1.4–1.5 GB |
| moli | 28.8 | 29.6 | 0.84–0.9 GB |
| Google Chrome | 11.6 | 11.7 | 2.2–2.4 GB |

Reproduce: `benchmarks/crawl/engines.sh` (set `ENGINES=auto` for the default chain).

## 9. A production workload: replacing Crawl4AI

[AiSeek](https://aiseek.dev) fetches news and company pages with Crawl4AI 0.9.4. We ran its own benchmark (`bench.py`) against both services with the production server's limits (6 cores, 10 GB memory, CPU weight 25), on 210 real production URLs (205 hosts) and 25 fixed pages. pagesift ran behind a small adapter implementing the same `/v1/fetch` contract ([`benchmarks/production/aiseek`](../benchmarks/production/aiseek)).

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

## 10. Deployment footprint

| | Crawl4AI | pagesift |
|---|---|---|
| Install | Python + 115 packages (~1 GB) + Playwright browsers (658 MB) | one 33 MB binary; Lightpanda (76 MB) for rendering |
| Docker image ↓ | 6.6 GB (AiSeek's, incl. its models) | TODO: measure the release image |
| Cold start ↓ | 1.3 s + 1.7 s browser launch | 0.12 s |

## 11. Office documents, EPUB and CSV to Markdown

**Date:** 2026-10-27, pagesift 0.80 (`pagesift-office`). **Dataset:** 42 development files and 39 hold-out files (Word, Excel, PowerPoint, EPUB, CSV) from Apache POI test data, GOV.UK publications, IDPF EPUB 3 samples, Project Gutenberg, csv-spectrum and datasets; the hold-out files were collected only after the rules were frozen. Ground truth: pandoc 3.9 candidates corrected with python-docx style rules, LibreOffice Calc display values and the EPUB XHTML, every file checked by hand. Rules: [`benchmarks/office/REVIEW.md`](../benchmarks/office/REVIEW.md).

Structure retained ↑ = kept ground-truth elements / all ground-truth elements (headings, tables, sampled cells, list items, image alt text, footnotes, speaker notes, heading order):

| Set | pagesift ↑ | MarkItDown 0.1.9 ↑ | pandoc 3.9 ↑ |
|---|---|---|---|
| Development (42 files) | **0.987** | 0.833 | 0.668 |
| Hold-out (39 files) | **0.988** | 0.717 | 0.705 |

Hold-out by format (pagesift / MarkItDown / pandoc): docx 1.000 / 0.842 / 0.570, xlsx 1.000 / 0.733 / 0.694, pptx 0.995 / 0.556 / 0.949, epub 0.961 / 0.669 / 0.633, csv 1.000 / 1.000 / 0.875. Excel cells that match LibreOffice's displayed value ↑: 0.9995 (MarkItDown 0.8985) on the hold-out set. MarkItDown scores higher on the development EPUBs (0.991 vs 0.957): pagesift writes one-column tables (tables of contents) as lines, by design.

The ground truth and the scorer were corrected after the first hold-out scoring (pandoc's spaces between text runs, table sizes taken from the sheet XML instead of LibreOffice's HTML, and Markdown equivalences such as hard breaks inside list items); the same corrections apply to every tool, and the scores before and after are both in the results file. Before any correction the hold-out scores were pagesift 0.937, MarkItDown 0.704, pandoc 0.676.

Speed and memory on all 81 files, same machine, one tool at a time inside the benchmark lock (2026-10-27): converting all of them takes pagesift **657 ms** in total versus MarkItDown's **48,819 ms** (time ↓, median of 5 / 3 in-process runs per file, summed); peak resident memory per file, each in its own process ↓, has a median of **14.2 MB** versus **140.1 MB** (process baselines 13.5 MB and 137.4 MB) and a maximum of **46.9 MB** versus **789.5 MB**.

Reproduce: [`benchmarks/office/README.md`](../benchmarks/office/README.md); full results: [`benchmarks/office/results-2026-10-27.md`](../benchmarks/office/results-2026-10-27.md).
