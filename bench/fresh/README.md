# Fresh release benchmark

The numbers in [docs/benchmarks.md](../../docs/benchmarks.md) for a release come from this run, made from scratch:
a new clone of the repository built from source, every competitor installed at its latest version, freshly
fetched pages, and a new random sample of sites. Nothing is reused from earlier runs.

Tools run one at a time on an otherwise idle machine. Every metric states which direction is better.

## 0. Setup

```bash
git clone https://github.com/YOLOW92/pagesift pagesift-fresh && cd pagesift-fresh
cargo build --release --locked -p pagesift --bins --examples
(cd bench/crawl && cargo build --release)            # local origin + Spider adapter (latest Spider)
(cd bench/compare/rust-tools && cargo update && cargo build --release)   # crw-extract, dom_smoothie
```

Competitors (latest versions), in `bench/data/fresh/tools/`:

- Python venv `py`: trafilatura, MarkItDown, newspaper4k, goose3, readability-lxml, jusText, boilerpy3, html2text,
  markdownify, inscriptis, Resiliparse, Scrapy
- Python venv `c4ai`: Crawl4AI (uses the system Chrome; no Playwright browser download)
- Node `node`: @mozilla/readability + turndown + jsdom, Defuddle, @extractus/article-extractor
- `crw`: the latest release binary
- browser engines: Lightpanda, moli, chrome-headless-shell

## 1. Data, fetched now

| Set | What | Used for |
|---|---|---|
| Firecrawl `scrape-content-dataset-v1` | 1,000 labelled URLs, re-fetched today (`bench/fetch_snapshots.py`), rendered snapshots for thin pages | extraction quality (truth/noise phrases), speed |
| Scrapinghub article-extraction-benchmark | 181 news pages with human-written article bodies (MIT) | extraction quality (token F1, its own `evaluate.py`) |
| Random sites | N sites sampled uniformly from today's Tranco top 200k (ranks 1k–200k), home page + 2 content pages each (`make_corpus.py`, seed recorded) | extraction speed, crawling corpus, rendering, real-world fetch |

## 2. Runs

1. **Extraction quality and speed**, every extractor on both labelled sets and the random corpus (single-thread
   latency, all-core throughput).
2. **HTTP crawling**, local origin serving the random corpus: `wide` (200 sites × 100 pages) and `huge`
   (1,000 sites × 50 pages), unthrottled; pagesift, crw, Spider, Scrapy, Crawl4AI (single and multi-process).
   Throughput, peak PSS of the whole process tree, CPU.
3. **Browser rendering**, same origin, every page rendered: pagesift (auto chain and each engine), Crawl4AI
   browser mode, crw with its renderer.
4. **Real-world fetch**, a fresh random sample of live URLs, each fetched once per tool (polite: robots.txt,
   one request per site per tool, spaced): success rate, final status codes, latency, text length.
5. **Footprint**: cold start, install size, Docker image size, dependency count.

Results land in `bench/fresh/results/<date>/` as JSON lines plus a generated summary.
