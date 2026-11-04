## Performance at a glance (PageSift 1.1)

PageSift 1.1 was re-measured; competitor numbers are the recorded 2026-10-21/22/27/28 data and were not re-run. One row per workload, best PageSift configuration; memory is a column, not a separate rank.


### Far ahead

| Domain | Workload | Metric | PageSift 1.0 | Fastest rival | Rival | Gap | Peak MB/worker (PageSift / rival) |
|---|---|---|---|---|---|---|---|
| PDF → Markdown | olmOCR-bench 1,269 pages, per page (single thread) | ms/page | 2.4 | pypdf | 50.0 | ×20.49 | — |
| HTML → Markdown | 600 generated documents, per page | ms/page | 0.24 | Turndown | 1.9 | ×8.12 | — |
| Main content | Firecrawl 597 pages, single thread | p50 ms/page | 1.0 | crw | 7.4 | ×7.26 | — |
| Publication dates | htmldate 1,000 pages, date search | ms/page | 0.31 | htmldate (fast) | 2.0 | ×6.45 | — |
| RAG chunking | 5 corpora, 1.4 MB | total ms | 7 | chonkie recursive | 35 | ×5.01 | — |
| Office / e-books | B4 81 files, in-process | files/s | 91.7 | xberg | 27.0 | ×3.40 | 47 / 112 |
| PDF → Markdown | B1 144 pages, 1 worker, resident server | files/s | 176.5 | pdf_oxide | 100.6 | ×1.75 | 211 / 29 |
| RAG query filter | 472 questions, default settings | answer recall | 0.929 | Crawl4AI BM25 | 0.673 | +0.256 | — |
| Wild crawl | polite, 40 sites × 60 pages, coverage | coverage | 0.740 | Crawl4AI (HTTP) | 0.529 | +0.211 | — |
| Scanned PDFs | B5 134 pages, olmOCR score without baseline, OCR on | olmOCR score | 0.314 | Docling (GPU) | 0.125 | +0.189 | — |
| Wild crawl | fast, 40 sites × 60 pages, coverage | coverage | 0.687 | Crawl4AI (HTTP) | 0.499 | +0.188 | — |
| PDF → Markdown | olmOCR-bench 1,269 pages | official score | 0.645 | pymupdf4llm | 0.484 | +0.161 | — |
| Scanned PDFs | B5 134 pages, olmOCR score with baseline, OCR on | olmOCR score | 0.543 | Docling (GPU) | 0.410 | +0.133 | — |
| Office / e-books | B4 hold-out 39 files, structure kept | structure kept | 0.988 | xberg | 0.878 | +0.110 | — |


### Ahead

| Domain | Workload | Metric | PageSift 1.0 | Fastest rival | Rival | Gap |
|---|---|---|---|---|---|---|
| HTML → Markdown | 600 generated documents, fidelity | mean F1 | 1.000 | markdownify | 0.936 | +0.064 |
| Hard PDFs | B3 100 pdf.js files, completion | completion | 0.720 | pymupdf4llm | 0.680 | +0.040 |
| Main content | WCXB test split, 150 pages | F1 | 0.866 | Readability + Turndown | 0.848 | +0.018 |



Largest gaps: fast, 40 sites × 60 pages, time (spider covered about half as many pages) 16.1 vs spider 1.4 s; A2 replay, 269 pages, HTTP only, no rate limit, 1 worker 47.5 vs spider 156.6 pages/s; A2 269 random pages, resident worker, per page 4.5 vs Resiliparse 2.1 ms/page. Full table: [report](report.md).


A2 throughput is measured against a replay server on a single loopback address, so all 269 pages count as one host. PageSift waits 1 s between requests to the same host by default; spider has no such limit. This row mostly reflects that politeness interval, not engine speed.
