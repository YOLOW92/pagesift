# Benchmark History — 1.4.0

Archived measurement run for PageSift `1.4.0` (`2026-11-14`).

## Machine & Toolchain

- **CPU**: Intel(R) Core(TM) i7-10700 CPU @ 2.90GHz
- **Memory**: 15 GiB RAM
- **GPU**: NVIDIA GeForce GTX 1070 (driver `580.178.04`)
- **OS**: Linux `7.0.0-43-generic`
- **Rust**: `rustc 1.99.0` (`cargo 1.99.0`), non-PGO unstripped release binary (`49.41 MB`, `51,813,752` bytes)

## Measurement Scope

Only PageSift `1.4.0` was re-measured on available local offline workloads; `1.0` results (`benchmarks/history/1.0/results/final/new/metrics.tsv`) were copied to `results/final/old/metrics.tsv` and `186` metrics for deleted offline corpora or live-web suites (`a1`, `a2`, `a4`, `b2`, `extract`, `dates`, `rag`, `crawl`, `wild`, `a3`, `browser`, `a2fast`) were carried forward into `results/final/new/metrics.tsv`. Raw newly measured values (`231` keys) are preserved separately in `results/final/new/metrics.measured.tsv`.

- **Re-measured in 1.4.0**:
  - `v2quality`: B1 (1,269 PDF pages), B3 (100 pdf.js files), B4 (`yolowdoc 0.1.1`, 81 Office/e-book files / 39 holdout), B5 (134 scanned PDF pages).
  - `v2speed`: B1 (`MIX_SUBSET=model`, 144 PDF pages) and B4 (81 Office/e-book files) on CLI (`1w`/`16w`) and persistent `serve` (`1w`/`16w`) with warmed OS page cache.
  - `b3b5`: B3 and B5 CLI throughput (`1w`/`16w`) and peak RSS with warmed OS page cache.
  - `b5ocr`: GPU scanned PDF OCR on GTX 1070 (`small_math_ocr`, `small_math_auto`, `medium_math_ocr`).
  - `pdf`, `office`, `markdown` (600 docs across 3 seeds), `api` (localhost Firecrawl v1/v2 & Jina Reader endpoints; `firecrawl-py` SDK skipped), `formats` (373 source languages + SQLite UTF-8/UTF-16).
  - `yolow_deps`: standalone `yolowsyntax 0.1.2` (8 real OSS source files, single-thread pinned) and `yolowzstd 0.1.2` (Silesia 4 files: `dickens`, `mozilla`, `ooffice`, `xml`).
  - `datagraph`: `pagesift-graph` A/B overhead (`181` HTML pages from `benchmarks/data/six/aeb`), `StreamingGraphBuilder` (`1,000` docs), and 2-version local HTTP crawl E2E (`scrape` + `crawl` + `graph` deltas). `10M`-scale graph test was skipped in this run.
- **Baselines retained**:
  - `xberg 1.3.9` (`2026-11-04`, imported from `benchmarks/history/1.1/results/xberg-1.3.9`)
  - Competitor baselines from `2026-10-21/22/27/28`

## Reproduction

```bash
RUN_ID=final ./benchmarks/history/1.4/run.sh all
python3 -I benchmarks/history/1.4/score/publish.py benchmarks/history/1.4/results/final/new/scorecard.json --stamp final --write
```
