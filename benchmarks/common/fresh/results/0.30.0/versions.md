# Benchmark Environment & Competitor Versions (2026-10-23 — `pagesift 0.30.0`)

## Target Engine
- **pagesift**: `0.30.0` (`/home/yolow92/pagesift/target/release/pagesift`, commit `17a82ac9`)

## Newly Added Competitors in `0.30.0` Suite
- **htmd** (`letmutex/htmd`): `0.5.6` (Rust crate in `pagesift-compare-rust`)
- **html-to-markdown-rs** (`kreuzberg-dev/html-to-markdown` / `xberg-io/html-to-markdown`): `3.18.6` (Rust crate in `pagesift-compare-rust`)
- **go-readability** (`go-shiori/go-readability`): `v0.0.0-20251205110129-5db1dc9836f0` (Go module in `pagesift-compare-go`)
- **JohannesKaufmann/html-to-markdown** (`go-html-to-markdown`): `v2.5.2` (Go module in `pagesift-compare-go`)
- **Colly** (`gocolly/colly/v2`): `v2.3.0` (Go binary `/home/yolow92/pagesift-bench-env/tools/bin/colly_crawl`)
- **mdream** (`harlan-zw/mdream`): `2.0.3` (Node package in `/home/yolow92/pagesift-bench-env/tools/node`)
- **jina-reader** (`jina-ai/reader` pipeline: `@mozilla/readability@0.6.0` + `turndown@7.2.4` + `turndown-plugin-gfm@1.0.2`)
- **crawlee-python** (`apify/crawlee-python` `crawlee[beautifulsoup]`): `1.11.0` (Python package in `tools/py` and `benchmarks/data/six/venv`)

## Python Environment (`/home/yolow92/pagesift-bench-env/tools/py` — Python 3.14.4)
```text
bm25s                    0.3.14
boilerpy3                1.0.7
charset-normalizer       3.5.2
chonkie                  1.7.0
chonkie-core             0.10.2
crawlee                  1.11.0
fuzzysearch              0.8.2
gallery-dl               1.32.17
goose3                   3.1.22
html2text                2025.4.15
htmldate                 1.11.0
inscriptis               2.7.5
jusText                  3.0.2
langchain-text-splitters 1.1.3
lxml                     6.1.3
lxml-html-clean          0.4.5
markdownify              1.2.3
markitdown               0.1.9
newspaper4k              0.9.6
pdfplumber               0.11.10
playwright               1.64.1
pymupdf-layout           1.28.2
pymupdf4llm              1.28.2
pypdf                    6.21.2
pypdfium2                5.14.0
rapidfuzz                3.15.2
readability-lxml         0.9
Resiliparse              1.0.9
scrapling                0.5 (locked source: 3a8bf4e4d3ed91257ba0aa5b7dc66241d1e23840)
Scrapy                   2.19.0
semchunk                 4.1.1
trafilatura              2.3.1
```

## Crawl4AI Python Environment (`/home/yolow92/pagesift-bench-env/tools/c4ai` — Python 3.14.4)
```text
Crawl4AI                 0.9.5
```

## Six-Competitor Python Environment (`/home/yolow92/pagesift/benchmarks/data/six/venv` — Python 3.14.4)
```text
crawlee                  1.11.0
firecrawl-py             4.30.1
gallery_dl               1.32.17 (locked source: 61971e01d394d35c81a86d4d0d03717c78ae53d5)
lxml                     6.1.3
markdownify              1.2.3
scrapling                0.5 (locked source: 3a8bf4e4d3ed91257ba0aa5b7dc66241d1e23840)
```

## Node.js Environments (`v24.21.0`)
- `/home/yolow92/pagesift-bench-env/tools/node`:
```text
├── @extractus/article-extractor@9.0.1
├── @mozilla/readability@0.6.0
├── crawlee@3.18.2
├── defuddle@0.19.5
├── jsdom@30.1.3
├── mdream@2.0.3
├── turndown@7.2.4
└── turndown-plugin-gfm@1.0.2
```
- `/home/yolow92/pagesift/benchmarks/data/six/node`:
```text
├── @crawlee/cheerio@3.18.2
├── @crawlee/playwright@3.18.2
├── @mendable/firecrawl-js@4.28.1
└── playwright@1.64.1
```

## Native, Rust & Go Competitor Binaries / Crates
- **crw CLI**: `crw 0.39.0` (`/home/yolow92/pagesift-bench-env/tools/crw/crw` and `/home/yolow92/pagesift-bench-env/tools/bin/crw`)
- **Spider**: `spider 2.53.9` + `spider_transformations 2.39.13` (`/home/yolow92/pagesift-v0.20.0/bench/crawl/target/release/spider_crawl`)
- **pagesift-compare-rust** (`/home/yolow92/pagesift-bench-env/rust-tools/target/release/pagesift-compare-rust`):
  - `crw-core = 0.39.0`
  - `crw-extract = 0.39.0`
  - `dom_smoothie = 0.18.2`
  - `htmd = 0.5.6`
  - `html-to-markdown-rs = 3.18.6`
- **pagesift-compare-go** (`/home/yolow92/pagesift-bench-env/tools/bin/pagesift-compare-go` — Go `1.26.2`):
  - `github.com/go-shiori/go-readability v0.0.0-20251205110129-5db1dc9836f0`
  - `github.com/JohannesKaufmann/html-to-markdown/v2 v2.5.2`
- **colly_crawl** (`/home/yolow92/pagesift-bench-env/tools/bin/colly_crawl` — Go `1.26.2`):
  - `github.com/gocolly/colly/v2 v2.3.0`
- **Katana**: `v1.8.0` (SHA `35267ac5c8ff1db9694a319d0eb466ed97b0969f`, binary `/home/yolow92/pagesift/benchmarks/data/six/bin/katana`)
- **Essence**: `v0.3.0` (SHA `a4f845c6b98d3337106e74fe7b91f94d61ab599d`, binary `/home/yolow92/pagesift/benchmarks/data/six/bin/essence`, component adapter `/home/yolow92/pagesift/benchmarks/data/six/bin/offline-stable`)
- **Firecrawl**: `v2.11.0` (SHA `ef12eb36b2f3382838dfe0a0c1a5add3d5df7fe5`, converter `/home/yolow92/pagesift/benchmarks/data/six/bin/firecrawl-converter`)

## Browser Engines (`/home/yolow92/pagesift/benchmarks/data/tools`)
- **Google Chrome**: `155.0.8059.84`
- **chrome-headless-shell**: `156.0.8078.47`
- **moli**: `1.1.18`
- **Lightpanda**: `lightpanda-x86_64-linux`
- **Obscura**: `obscura`

## Toolchain & System
- **Rust**: `rustc 1.99.0 (b940084d7 2026-09-28)`
- **Go**: `go1.26.2 linux/amd64`
- **OS / Kernel**: `Linux 7.0.0-38-generic x86_64`
- **Memory**: `15 GiB RAM` (Benchmarks strictly capped at **12 GiB** via `MemoryMax=12G` / `MEASURE_MAX_MB=12288`)
