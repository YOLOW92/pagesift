# 0.80.2 verification (read-only)

Reproduction records and measurements behind
[docs/releases/0.80.2.md](../../../docs/releases/0.80.2.md). Everything here is synthetic and
generated in code; no corpus and no network are needed. Linux x86_64 container, 4 vCPUs, no IPv6,
no outbound access to the real university sites, release builds of this repository.

| Path | What it is |
| :--- | :--- |
| `results/focus-0.80.1.txt` | `cargo run --release -p pagesift --example focus_gate` on the 0.80.1 crawler with the 8 synthetic campus sites of 0.80.2 (the gate did not exist in 0.80.1; its sites were written against the tester's 159-site report, see the release notes): 7 useful pages of 96 |
| `results/focus-0.80.2.txt` | The same gate on 0.80.2, now 10 sites (8 campuses + a SaaS developer portal + a city services site, each with its own query), in its three modes: `--adaptive` (default), `--attachments`, `--no-adaptive` |
| `results/perf-0.80.1.tsv`, `results/perf-0.80.1-csv2.tsv`, `results/perf-0.80.1-crawl.tsv` | The 0.80.1 baseline from `benchmarks/history/0.80.1/bench.sh` on this machine (same files as in that folder) |
| `results/perf-0.80.2.tsv` | The same script's steps on 0.80.2: `perf_gate` ×3, `pdf_one` ×3 plus callgrind instructions, `office_bench` plus instructions, `crawl_gate` ×3 |
| `results/repro-pdf-0.80.2.txt` | `POST /v2/parse` on the two PDF follow-ups: a bookmarked paper without `/Title` (S-4) and `paper-rootlost-55.pdf` (D-2) |
| `paper-rootlost-55.pdf` | An 8-page linearized paper (pikepdf, `benchmarks/history/0.80.1/gen_fixtures.py` input) with its object stream blanked and the file cut at 55%: the page tree root is gone |

Performance against 0.80.1 (instruction counts are the stable measure on a shared machine;
wall-clock medians are listed in the files):

| Step | 0.80.1 | 0.80.2 | Δ |
| :--- | ---: | ---: | ---: |
| `pdf_one paper.pdf` instructions | 2,207,402,868 | 2,206,525,819 | −0.04% |
| `office_bench ascii.csv` instructions | 4,318,530,413 | 4,318,532,851 | +0.00% |
| `office_bench sjis.csv` instructions | 1,625,363,985 | 1,625,366,457 | +0.00% |
| `office_bench gbk.csv` instructions | 1,794,052,640 | 1,794,055,065 | +0.00% |
| `office_bench utf8-cjk.csv` instructions | 1,182,424,119 | 1,182,426,573 | +0.00% |
| `perf_gate` wiki 250 KB median, A/B alternating 3× each | 28.2 / 28.7 / 27.8 ms | 27.7 / 28.4 / 27.8 ms | within noise |
| `crawl_gate` 20×60 pages/s, 3 runs | 646–701 | 650–708 | within noise |
| `focus_gate` CPU time, 8 campuses | 0.18 s | 0.14 s | −22% |

`crawl_gate` reports FAIL on this container for both versions: its 1500 pages/s floor was set on
the maintainer's machine, and this 4-vCPU container reaches about 700 in both. Peak RSS (31–33 MB,
limit 120) passes.
