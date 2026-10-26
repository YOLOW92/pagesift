# Main-content extraction

Summary and tables: [docs/benchmarks.md §1](../../docs/benchmarks.md#1-main-content-extraction-quality-and-speed).

| Path | What it is |
| :--- | :--- |
| [compare/](compare/) | Adapters for every compared tool and how to rerun the head-to-head comparison ([README](compare/README.md)) |
| [webmainbench/](webmainbench/) | WebMainBench, WCEB and olmOCR scoring drivers used by the 0.70 baselines |
| [vendor/](vendor/README.md) | Upstream scorers (WebMainBench, MinerU-HTML) pinned at the commits listed in their README |
| [adapters/goadapter/](adapters/goadapter/) | Go adapter for go-readability and html-to-markdown |
| [scripts/](scripts/) | 0.70 D1 (WCXB), D4 (DOM relocation) and D6 (charset) harnesses |

The AEB, WCXB and WebMainBench scorers used by the release runs are in
[../common/fresh/](../common/fresh/) (`aeb.py`, `wcxb.py`, `webmain.py`).

## Install

- Python competitors (trafilatura, MarkItDown, newspaper4k, goose3, readability-lxml, jusText, boilerpy3,
  html2text, inscriptis, Resiliparse) and Crawl4AI: the shared `py` and `c4ai` environments; Readability +
  Turndown and Defuddle: the `node` environment ([Tool environments](../README.md#tool-environments)).
- Rust competitors (crw-extract, dom_smoothie): `cargo build --release --manifest-path benchmarks/web-extraction/compare/rust-tools/Cargo.toml`.
- pagesift: `cargo build --release -p pagesift` and `cargo build --release -p pagesift-content --example extract_jsonl`.
- Optional — Go adapter (go-readability, html-to-markdown; used only by the 0.70 D1 run): needs Go
  (`go.mod` asks for 1.25), then `cd benchmarks/web-extraction/adapters/goadapter && go build`; library
  versions are in `go.mod`. Without it those two tools are simply not run.
- Data: the Firecrawl set and its snapshots (`compare/README.md` step 1, `benchmarks/data/1-0-0.csv`,
  `benchmarks/data/snapshots`), AEB in `benchmarks/data/fresh/data/aeb`, WCXB via `scripts/dl_wcxb.sh`.

To rerun everything as in the release runs: `benchmarks/common/fresh/run_extract.sh <result dir>`. It needs
the whole machine, and its `prep` step rewrites `benchmarks/data/inputs`.

## Dry run (2026-10-27)

On the first 3 pages of the frozen Firecrawl inputs: `py_adapter.py` (trafilatura, MarkItDown),
`crawl4ai_adapter.py`, `js_adapter.mjs` (Readability, Defuddle), `pagesift-compare-rust` (crw, dom_smoothie),
`extract_jsonl` and `parallel.sh` all wrote 3 records; `pagesift bench benchmarks/data --auto --external <file>`
scored such a file. Problems found:

- The node argument of `js_adapter.mjs` is the directory that holds `node_modules/`, not `node_modules/`
  itself; `compare/README.md` said otherwise and is corrected.
- Go is not installed on the benchmark machine, so the optional `goadapter` was not built or run.
- The frozen manifest `benchmarks/data/inputs/manifest.jsonl` stores paths as `bench/data/inputs/…`.
  They resolve only while `bench/data` points at the data directory (a symlink is enough); regenerate the
  inputs (`compare/README.md` step 2) to drop that.
