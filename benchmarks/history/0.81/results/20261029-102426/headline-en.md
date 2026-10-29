## Performance at a glance (PageSift 0.81)

PageSift 0.81 was re-measured; competitor numbers are the recorded 2026-10-21/22/27/28 data and were not re-run. One row per workload, best PageSift configuration; memory is a column, not a separate rank.


### Far ahead

| Domain | Workload | Metric | PageSift 0.81 | Fastest rival | Rival | Gap | Peak MB/worker (PageSift / rival) | Rival data |
|---|---|---|---|---|---|---|---|---|
| PDF → Markdown | olmOCR-bench 1,269 pages, per page (single thread) | ms/page | 2.2 | pypdf | 50.0 | ×22.32 | — | 2026-10-22 |
| HTML → Markdown | 600 generated documents, per page | ms/page | 0.24 | Turndown | 1.9 | ×8.24 | — | 2026-10-21 |
| Main content | Firecrawl 597 pages, single thread | p50 ms/page | 1.0 | crw | 7.4 | ×7.23 | — | 2026-10-21 |
| Publication dates | htmldate 1,000 pages, date search | ms/page | 0.31 | htmldate (fast) | 2.0 | ×6.45 | — | 2026-10-21 |
| Office / e-books | B4 81 files, in-process | files/s | 94.1 | Kreuzberg | 19.5 | ×4.82 | 38 / 231 | 2026-10-28 |
| RAG chunking | 5 corpora, 1.4 MB | total ms | 11 | chonkie recursive | 35 | ×3.24 | — | 2026-10-21 |
| PDF → Markdown | B1 144 pages, 1 worker | files/s | 258.5 | pdf_oxide | 100.6 | ×2.57 | 163 / 29 | 2026-10-28 |
| RAG query filter | 472 questions, default settings | answer recall | 0.929 | Crawl4AI BM25 | 0.673 | +0.256 | — | 2026-10-21 |
| Wild crawl | polite, 40 sites × 60 pages, coverage | coverage | 0.740 | Crawl4AI (HTTP) | 0.529 | +0.211 | — | 2026-10-21 |
| Wild crawl | fast, 40 sites × 60 pages, coverage | coverage | 0.685 | Crawl4AI (HTTP) | 0.499 | +0.186 | — | 2026-10-21 |
| Office / e-books | B4 hold-out 39 files, structure kept | structure kept | 0.988 | Kreuzberg | 0.816 | +0.172 | — | 2026-10-28 |
| PDF → Markdown | olmOCR-bench 1,269 pages | official score | 0.628 | pymupdf4llm | 0.484 | +0.144 | — | 2026-10-28 |


### Ahead

| Domain | Workload | Metric | PageSift 0.81 | Fastest rival | Rival | Gap | Peak MB/worker (PageSift / rival) | Rival data |
|---|---|---|---|---|---|---|---|---|
| PDF → Markdown | B1 144 pages, 16 workers | files/s | 580.6 | pdf_oxide | 501.7 | ×1.16 | 14 / 11 | 2026-10-28 |
| Main content | A2 269 random pages, 16 workers | pages/s | 630.0 | Resiliparse | 559.3 | ×1.13 | 17 / 54 | 2026-10-28 |
| HTML → Markdown | 600 generated documents, fidelity | mean F1 | 1.000 | markdownify | 0.936 | +0.064 | — | 2026-10-21 |
| Main content | WCXB test split, 150 pages | F1 | 0.866 | Readability + Turndown | 0.848 | +0.018 | — | 2026-10-28 |



Largest gaps: A2 replay, 269 pages, 1 worker 2.4 vs spider 156.6 pages/s; fast, 40 sites × 60 pages, time 16.6 vs spider 1.4 s; B5 134 pages, completion 0.112 vs Docling (GPU) 1.000. Full table: [report](report.md).
