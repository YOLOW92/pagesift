# PDF → Markdown

Data: [olmOCR-bench](https://huggingface.co/datasets/allenai/olmOCR-bench) (ODC-BY) and its scoring code (Apache-2.0,
`allenai/olmocr`, `olmocr/bench`). Only the subsets made of born-digital PDFs are used: `arxiv_math`,
`headers_footers`, `long_tiny_text`, `multi_column`, `table_tests` (the others are scans, which need OCR). Download the
`*.jsonl` files and `pdfs/` into `benchmarks/data/fresh/data/olmocr-bench`, the `olmocr` repository into
`benchmarks/data/fresh/tools/olmocr-bench`, and install the competitors into the venv `benchmarks/data/fresh/tools/py`.

Each test checks one thing on one page: a sentence is present, a header or footer is absent, two sentences are in
order, a table cell has the right neighbours, or a formula renders like the reference in KaTeX. A tool's score is
the share of tests it passes (higher is better).

```bash
benchmarks/pdf/run.sh                 # all tools; or benchmarks/pdf/run.sh pagesift pypdf
```

- `pdf_dir` (pagesift) and `py_tools.py` (pymupdf4llm, pdfplumber, pypdf, MarkItDown; default settings) convert every
  PDF and print the mean time per page.
- `score.sh <tool> [subset ...]` runs the benchmark's own `benchmark.py`.
- `bench_fast.py` is `benchmark.py` with a pre-filter for the math tests: before rendering a candidate formula in
  KaTeX it checks that the candidate's LaTeX source contains every ASCII letter and digit of the reference's
  rendering (a rendered letter or digit always appears in the source). Candidates that fail cannot pass and are
  skipped, as unrenderable candidates are in the original. Same results, the math subset scores in 3 minutes
  instead of hours.
- `diff.py <subset> <toolA> <toolB>` compares two tools test by test, on the tuning and hold-out halves.

pymupdf4llm is AGPL-licensed and is only run for evaluation. Results: [results-2026-10-22.md](results-2026-10-22.md).

## Install

- Competitors (pymupdf4llm, pdfplumber, pypdf, MarkItDown) are in the shared `py` environment
  ([Tool environments](../README.md#tool-environments)).
- Scoring code: the `allenai/olmocr` repository in `benchmarks/data/fresh/tools/olmocr-bench`
  (`score.sh` runs `olmocr/bench` from there with the `py` environment).
- Data: `scripts/dl_olmocr_1403.sh` downloads the whole olmOCR-bench (1,403 PDFs, including the scanned
  subsets that are not scored) into `benchmarks/data/olmocr_1403/bench_data` with `huggingface_hub`
  (not installed in the system Python on the benchmark machine: install it first, e.g. into the `py`
  environment). `run.sh` reads `benchmarks/data/fresh/data/olmocr-bench` unless `DATA` points elsewhere:

```bash
DATA=benchmarks/data/olmocr_1403/bench_data benchmarks/pdf/run.sh pagesift
```

## Dry run (2026-10-27)

The benchmark machine has the data only in `olmocr_1403/bench_data`, not in `fresh/data/olmocr-bench`,
so `run.sh` as written stopped at the data path (hence the `DATA` override). With that data: `pdf_dir`
converted all 1,403 PDFs (0 failed, 3.42 ms per file) and `score.sh pagesift headers_footers` scored
the subset; `py_tools.py` ran pypdf and pdfplumber on 3 PDFs. No full scoring run.
