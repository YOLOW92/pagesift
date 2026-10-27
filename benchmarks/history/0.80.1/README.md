# 0.80.1 fix verification (read-only)

Reproduction records and the performance comparison behind
[docs/releases/0.80.1.md](../../../docs/releases/0.80.1.md). Everything here is synthetic and
generated in code; no corpus and no network are needed.

| Path | What it is |
| :--- | :--- |
| `gen_fixtures.py` | Writes the fixtures: `paper.pdf` (40 pages, author line with CMSY10 footnote asterisks), `paper-untitled.pdf` (no `/Info /Title`), and the ASCII, Shift_JIS, GBK and UTF-8 CJK CSVs. `python3 -I gen_fixtures.py <dir> 40` |
| `bench.sh` | Runs `perf_gate`, `pdf_one`, `office_bench` and `crawl_gate` from a release build on those fixtures; wall-clock medians plus `valgrind --tool=callgrind` instruction counts |
| `results/repro-0.80.0.md` | How each review item was reproduced on 0.80.0 and re-checked on 0.80.1 (Linux x86_64 container, 4 vCPUs, no IPv6) |
| `results/perf-0.80.0.tsv`, `results/perf-0.80.1.tsv` | The two full runs (the 0.80.1 run predates the sampled CSV detection) |
| `results/perf-0.80.1-csv2.tsv` | The CSV numbers of the final 0.80.1 code (UTF-8 fast path, 64 KiB detection sample) |
| `results/perf-0.80.0-crawl.tsv`, `results/perf-0.80.1-crawl.tsv` | `crawl_gate`, three runs each; 0.80.0 ran with `s0.localhost`…`s19.localhost` added to `/etc/hosts` |

The `paper-truncated.pdf` used for the damaged-PDF reproduction is `paper-untitled.pdf` cut at
byte 19567 (inside the third page object), as described in `results/repro-0.80.0.md`.
