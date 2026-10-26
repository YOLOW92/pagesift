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
