# 0.93 element relocation: pagesift `locate` vs Scrapling `adaptive`

Measured 2026-11-03 on one cloud machine (4 vCPU, Linux), pagesift `bench_relocation` example built
with `--profile release-fast`, Scrapling 0.5 (`Selector(..., adaptive=True)`, `css(..., auto_save=True)`
on the old page, `css(..., adaptive=True)` on the new one, default `percentage=40`).

## Data

| Step | Script | Output (not in git) |
|---|---|---|
| Corpus | `fetch.py` | `benchmarks/data/0.93/live/` (27 current pages, robots.txt checked, 2 s between requests), `benchmarks/data/0.93/cc/` (24 URLs present in both Common Crawl `CC-MAIN-2017-13` and `CC-MAIN-2024-10`) |
| Cases | `generate.py` (seed 930) | `benchmarks/data/0.93/cases.jsonl`, 1463 cases |
| Run | `run.py` | `results/summary.json` |

Common Crawl replaces the Wayback Machine for real redesign pairs: `web.archive.org` and
`index.commoncrawl.org` are not reachable from the measuring machine, `data.commoncrawl.org` is.
`cc.py` binary-searches `cluster.idx` with HTTP Range reads and fetches single WARC records.

- **Selector**: the one a scraper author would write for the element — from the nearest `id`,
  `tag.class` steps with `:nth-of-type` only where siblings need it. Selectors through `table > tr`
  are dropped (lxml does not insert `<tbody>`, so they would never match in an HTML5 parser).
- **Synthetic** (6 kinds × 8 elements per live page): `hash_classes` (every class replaced by a
  generated hash), `wrap` (an extra `div` around the parent), `retag` (span→div, h2→h3, p→div …),
  `drop_attrs` (id and class removed from the element and three ancestors), `insert_siblings` (two
  look-alike siblings with the same tag and attributes inserted before it), `combo` (hash + wrap +
  siblings). The expected answer is the element's unchanged text.
- **Real**: an element of the 2017 page whose text is the whole text of exactly one element in the
  2024 page; that element is the answer.
- **Split**: by page URL (seeded hash) into tuning (547 cases) and holdout (916). `locate` has no
  tunable thresholds (δ = 0.05 is fixed); the tuning half was only used to compare the two decision
  rules described in `pagesift-extract/src/locate/posterior.rs`.

## Holdout results (916 cases)

| Metric | Direction | pagesift | Scrapling |
|---|---|---:|---:|
| correct rate | ↑ | **53.3%** | 46.5% |
| wrong rate (silently returned another element) | ↓ | **18.7%** | 53.5% |
| abstain rate (missing / ambiguous) | — | 28.1% | 0.0% |
| time per field, median (parse both pages, remember, find again) | ↓ | **2.9 ms** | 36.1 ms |
| time per field, p95 | ↓ | **74.9 ms** | 419.5 ms |

By change kind (holdout; correct / wrong):

| Kind | n | pagesift | Scrapling |
|---|---:|---|---|
| hash_classes | 144 | 69.4% / 4.9% | 78.5% / 21.5% |
| wrap | 144 | 57.6% / 20.1% | 66.0% / 34.0% |
| retag | 144 | 40.3% / 25.0% | 30.6% / 69.4% |
| drop_attrs | 144 | 50.0% / 6.3% | 41.0% / 59.0% |
| insert_siblings | 144 | 40.3% / 45.1% | 0.0% / 100% |
| combo | 144 | 57.6% / 11.1% | 53.5% / 46.5% |
| real (Common Crawl 2017 → 2024) | 52 | 65.4% / 17.3% | 73.1% / 26.9% |

Of pagesift's 171 wrong answers on the holdout, 137 come from step ① (the old selector still matches
exactly one element, but a different one — typically `:nth-of-type` after siblings were inserted);
34 (3.7% of all cases) come from the fingerprint step. Both tools use the old selector first when it
matches.
