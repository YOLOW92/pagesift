# Benchmarks

Scripts, adapters and dated results behind every number in [docs/benchmarks.md](../docs/benchmarks.md),
grouped by domain. Each metric there says which direction is better; each domain directory holds the
method, the scripts to rerun it and the result files.

| Domain | Directory | Summary |
| :--- | :--- | :--- |
| Main-content extraction | [web-extraction/](web-extraction/) | [§1](../docs/benchmarks.md#1-main-content-extraction-quality-and-speed) |
| PDF → Markdown | [pdf/](pdf/) | [§5](../docs/benchmarks.md#5-pdf-to-markdown) |
| Word / Excel / PowerPoint / EPUB / CSV | [office/](office/) | [§11](../docs/benchmarks.md#11-office-documents-epub-and-csv-to-markdown) |
| Crawling, robots.txt, browser rendering | [crawl/](crawl/) | [§6](../docs/benchmarks.md#6-crawling-over-http-no-rate-limit), [§7](../docs/benchmarks.md#7-crawling-messy-real-world-sites-coverage-waste-and-politeness), [§8](../docs/benchmarks.md#8-browser-rendering) |
| Scale and multi-core scaling | [scale/](scale/) | [200M-request run](../docs/history/0.70_200m_regression.md) |
| Production workload and service API | [production/](production/) | [§9](../docs/benchmarks.md#9-a-production-workload-replacing-crawl4ai), [§10](../docs/benchmarks.md#10-deployment-footprint) |
| Chunking and query filtering for RAG | [rag/](rag/) | [§4](../docs/benchmarks.md#4-chunking-and-query-filtering-for-rag) |
| Publication dates and metadata | [dates/](dates/) | [§2](../docs/benchmarks.md#2-publication-dates) |
| HTML → Markdown fidelity | [markdown/](markdown/) | [§3](../docs/benchmarks.md#3-html--markdown-fidelity) |
| Media streams and WARC | [media/](media/) | — |
| Shared scripts and the release-run harness | [common/](common/) | — |
| 0.70 and earlier frameworks and evidence (read-only) | [history/](history/) | [docs/history/](../docs/history/README.md) |

The mixed-enhancement benchmark is being written under [`bench/mix/`](../bench/mix/RULES.md) and moves here when it is done.

Large inputs (datasets, snapshots, tool environments) are not in the repository; scripts expect them
under `benchmarks/data/`, which is ignored by git.

## Tool environments

The competitor tools live in three environments under `benchmarks/data/fresh/tools/`. The versions
below are the ones recorded for the 2026-10-22 run
([`versions.md`](common/fresh/results/2026-10-22/versions.md)); the environments on the benchmark
machine were made with uv 0.13.4, Python 3.14.4 and Node 24.21.0, Rust 1.99.0.

```bash
T=benchmarks/data/fresh/tools
# Python tools (extraction, Markdown, dates, RAG, PDF, Scrapy/Crawlee crawls)
uv venv --python 3.14 $T/py
uv pip install --python $T/py/bin/python \
  bm25s==0.3.14 boilerpy3==1.0.7 chonkie==1.7.0 goose3==3.1.22 html2text==2025.4.15 htmldate==1.11.0 \
  inscriptis==2.7.5 jusText==3.0.2 langchain-text-splitters==1.1.3 markdownify==1.2.3 markitdown==0.1.8 \
  newspaper4k==0.9.6 pdfplumber==0.11.10 pymupdf-layout==1.28.2 pymupdf4llm==1.28.2 pypdf==6.21.1 \
  readability-lxml==0.9 Resiliparse==1.0.9 Scrapy==2.19.0 semchunk==4.1.1 trafilatura==2.3.1 \
  crawlee==1.11.0 charset-normalizer==3.5.2 chardet==7.7.0 extruct==0.18.0
# Crawl4AI in its own environment (its dependencies conflict with the tools above)
uv venv --python 3.14 $T/c4ai
uv pip install --python $T/c4ai/bin/python crawl4ai==0.9.5 markdown-it-py==4.2.0
$T/c4ai/bin/crawl4ai-setup        # Crawl4AI's own setup: installs its Playwright browser
# Node tools (Readability + Turndown, Defuddle, article-extractor, Crawlee, mdream)
mkdir -p $T/node && cd $T/node && npm init -y && npm install @extractus/article-extractor@9.0.1 \
  @mozilla/readability@0.6.0 defuddle@0.19.5 jsdom@30.1.3 turndown@7.2.4 turndown-plugin-gfm@1.0.2 \
  crawlee@3.18.2 mdream@2.0.3
```

The last four Python packages and the three Node packages after `turndown` are not in `versions.md`; their versions
are the ones installed in the environments on the benchmark machine on 2026-10-27, which have
since drifted from `versions.md` in two places (markitdown 0.1.9, pypdf 6.21.2).

Each domain README says which of these a benchmark needs, the extra data to download, the commands to
rerun it, and the result of a dry run on 2026-10-27 (small samples, to check that the commands start and
the paths are right; no full scoring runs).

Two things hold for every domain:

- The 0.70 scripts in the domains' `scripts/` directories (D1–D9, E1, Common Crawl) start with
  `cd ~/pagesift` and run against the checkout at `~/pagesift`, not the one they live in. Several run
  tools in the `pagesift-bench:*` Docker images, whose Dockerfiles were in the removed `bench/docker`.
- Some frozen manifests under `benchmarks/data` store paths as `bench/data/…`. They resolve only while
  `bench/data` points at the data directory (a symlink is enough) or after the inputs are regenerated.
