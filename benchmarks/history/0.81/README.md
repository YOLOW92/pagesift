# PageSift 0.81 performance refresh

This directory is the 0.81 re-measurement of every domain the 0.80 release used,
with **PageSift only**. Competitor numbers are the recorded 2026-10-21/22/27/28
runs; they are not re-run. The result is therefore a *0.81 performance refresh*,
not a new head-to-head.

The user's rule for this round:

- 0.81 is built from the published release tree; no product-code change.
- Every domain is re-run for 0.81, including quality.
- Inputs are the frozen v1/v2 samples. B2/B3/B5, which had been lost locally,
  are restored from their recorded sources and SHA-256 checked; a file whose SHA
  does not match is recorded as unavailable, not replaced.
- Quality baseline: 0.81 scores are compared with the old scores, and every
  0.81 output is archived by SHA-256. There is no 0.80.10 byte-comparison run.
- The final view is one merged two-tier table: **大幅领先 / Far ahead** first,
  **领先 / Ahead** second, one row per workload with the best PageSift
  configuration. Memory is a column, not a separate rank. Everything else
  (parity, behind, browser engines, A3, 10M scale, Deployment/API) stays in the
  detailed report.
- The report states the competitor dates on every row. A2 was re-fetched on
  2026-10-29; its absolute times are not directly comparable with the 10-28 A2.

## What runs

| Stage | Workload | Main metrics |
|---|---|---|
| `v2quality` | A1/A2/A4/B1/B2/B3/B4/B5, frozen v2 samples | completion, F1, olmOCR, structure kept |
| `v2speed` | A2 HTML + crawl replay, B1 PDF, B4 Office, 1/16 workers | p50, p95, items/s, peak MB |
| `extract` | Firecrawl 597 pages | truth recall, leak, score, p50, 16-thread pages/s |
| `dates` | htmldate 1,000 pages | F1, date-search ms/page |
| `markdown` | 600 generated documents | mean F1, ms/page |
| `rag` | Chroma 5 corpora, 472 questions | chunking recall, query-filter recall, ms |
| `pdf` | olmOCR-bench 1,269 born-digital pages | official score, ms/page |
| `office` | B4 81 files | structure kept, files/s, peak MB |
| `b3b5` | B2 71 / B3 100 / B5 134 pages | p50, files/s at 1/16 workers |
| `crawl` | wide 200×100, huge 1,000×50 | pages/s, peak PSS |
| `wild` | 40 sites × 60 pages, fast/polite | coverage, waste, time, memory |
| `a3` | 100 live JS pages | completion, Firefox-reference coverage (appendix) |
| `browser` | auto wide + Chrome smoke | pages/s, peak PSS (appendix) |
| `scale` | 10M local requests | sustained pages/s (appendix) |
| `api` | binary size, cold start, Firecrawl/Jina API | appendix |

## Timed run (about 90 minutes)

The clock starts when the 0.81 binary and all inputs are ready.

```bash
# once, before the clock:
benchmarks/history/0.81/prep.sh

# the timed run:
benchmarks/history/0.81/run.sh
```

`prep.sh` builds the workspace, crawl origin and scale runner; restores
A1/A4/B1/B3/B5 and re-downloads the 71 recorded B2 PDFs with SHA checks; checks
the Playwright Firefox reference browser; and smokes every entry point.
`run.sh` writes raw outputs to `$MIX/runs-081/<timestamp>/` and archives small
summaries, per-item JSONL and output hashes under
`benchmarks/history/0.81/results/<timestamp>/`.

The planned budget is: quality 5, v2 speed 7, domain speed/quality 9, crawl 5,
B2/B3/B5 8, A3 10, quality regression 13, browser 16, scale 2, API 3,
tables 10 = 88 minutes plus 2 minutes buffer. The timeout cut order is:
B2 → A1 quality → auto huge → 10M scale → API.

## Presentation rules

- A row is **far ahead** when PageSift is ≥1.75× faster or ≥+0.10 absolute
  quality/completion; **ahead** at 1.10–1.75× or +0.01–+0.10.
- One row per workload, using the best PageSift configuration (for example,
  resident `serve` for per-item latency, CLI for 16-worker throughput).
- Memory is a column in the headline and detailed tables wherever the same
  method recorded a peak; it never creates a second headline row for the same
  workload.
- The homepage keeps only the two tables. The largest gaps get one sentence
  with concrete numbers and a link to the full report.
- A3, browser engines, 10M scale and Deployment/API are appendix items: they
  either have no same-method competitor or no frozen comparable snapshot.

## Files

| Path | Purpose |
|---|---|
| `prep.sh` | build, restore inputs, preflight smoke |
| `run.sh` | timed orchestration and archive/report |
| `restore_inputs.py` | A1/A4/B3/B5 materialization, B2 SHA-checked re-download |
| `domains/*.sh` | one script per stage |
| `score/scorecard.toml` | one row per workload, frozen rival value and date |
| `score/summarize.py` | two-tier view + detailed report |
| `score/collect_*.py` | v2 quality/speed metrics |
| `score/hash_outputs.py` | SHA-256 archive of every output |
| `results/` | archived summaries, hashes, scorecard |

## Current status

The 0.81.0 source (`8c1c799`) is checked out and built with the release PGO
configuration. The timed run finished on 2026-10-29 in 25m45s; results, hashes
and the two-tier report are in
[`results/20261029-102426/`](results/20261029-102426/). A2 serve 1-worker p50
was re-measured five times after the run (median 3.06 ms, range 2.21–3.79 ms)
because the first pass ran under load; the report uses the re-measured median.
