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
