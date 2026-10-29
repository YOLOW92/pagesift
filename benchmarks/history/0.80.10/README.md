# 0.80.10 measurements

All runs on 2026-10-28, in the same container as 0.80.9.

## Adaptive page limit, 69 universities × five directions

Same sites, queries and grading as [0.80.9](../0.80.9/) (`sites.txt`, `grade.py`; robots.txt
ignored). `run.sh <binary> <out> <limit|auto>` runs `crawl --query … --adaptive` with a fixed
`--limit` or, with `auto`, without one.

| File | What |
| :--- | :--- |
| `results/0.80.10-auto/` | 0.80.10, no `--limit` (`index.jsonl` per direction and site, `stops.tsv`) |
| `results/0.80.9-limit100/` | 0.80.9 with `--limit 100`: where its stopping rules end when the limit is loose |
| `../0.80.9/results/0.80.9/` | 0.80.9 with `--limit 12` |
| `cmp.py` | per direction and overall, on the crawls every run reached |
| `simulate_giveup.py` | replays the `--limit 100` results to choose the "nothing to find" rule |
| `results/perf-instructions.txt` | callgrind instruction counts against 0.80.0 |

`python3 cmp.py ../0.80.9/results/0.80.9 results/0.80.9-limit100 results/0.80.10-auto` gives
the table in the [release notes](../../../docs/releases/0.80.10.md). A few crawls were still
running when the numbers were taken and are left out. The auto run had up to five directions
running at once (each site saw at most five crawls with different queries at the same time).

What the `--limit 100` run of 0.80.9 showed: pages 13–24 were as useful as pages 1–12 (U 21.5%
against 20.7%), pages 25–36 still 16%; 25 crawls used the whole 100 and 17 of them had no useful
page, because before anything relevant is found irrelevant pages do not count towards stopping.
`simulate_giveup.py`: giving up after a fixed number of pages costs too much (30 pages: 21% of the
pages saved, 8.3% of the useful ones lost); giving up after B pages in a row that were reached
only through links without a sign of the query does better (B = 10: about a quarter saved, 4–6%
lost) and was shipped as `2 × patience`.

## Extraction (AiSeek report)

394 saved pages (253 random pages, 124 Firecrawl-set snapshots, 17 report samples) converted by
0.80.9 and 0.80.10: outputs changed on the samples and on 4 other pages (a logo, a newsletter box,
a "recently viewed" panel and a document title prefix left out). `pagesift bench` on the Firecrawl
set: truth recall 0.505, noise leak 0.042 for both.

## Performance

Instruction counts against 0.80.0 stay within 2% (`results/perf-instructions.txt`).
