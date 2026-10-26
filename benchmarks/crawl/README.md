# Crawl showdown

Whole-site crawling under identical conditions: every tool crawls the same local sites,
follows the same links, obeys the same politeness rules, and is measured the same way.

```bash
# once: build the origin and the Spider adapter, and pagesift's driver
cargo build --release --manifest-path benchmarks/crawl/Cargo.toml
cargo build --release -p pagesift --example crawl_bench
# crw (AGPL-3.0, evaluation only): cargo install --locked --root benchmarks/data/tools crw-cli

benchmarks/crawl/run_all.sh            # all scenarios; or: run_all.sh polite fast scale
```

## Sites

`origin` serves `s0.localhost` … `s{H-1}.localhost` from one port (`*.localhost`
resolves to `::1`, so every site is a separate host for per-host politeness). Each site
has P pages whose bodies are real pages from the extraction corpus
(`benchmarks/data/crawl-pages`: corpus pages with no access barrier). Every original `href`
is renamed to `data-gone` (the name must not contain "href": some crawlers follow any
attribute containing it), and each page gets links to pages 3j+1…3j+3 and the start
page, so a breadth-first crawl from `/` reaches exactly P pages. A fixed per-response
delay simulates network latency. `robots.txt` allows everything.

## Scenarios

| Scenario | Sites × pages | Per-host interval | Per-host concurrency | Latency |
|---|---|---|---|---|
| wide (default) | 200 × 100 | 0 | 8 | 10 ms |
| huge (default) | 1000 × 50 | 0 | 4 | 10 ms |
| polite (gate check only) | 100 × 50 | 1000 ms | 1 | 50 ms |

`wide` and `huge` have no rate limit and a network ceiling far above any tool, so they
measure what each crawler itself can do; `huge` stresses memory with many sites at once.
`polite` is the release-gate condition ("same politeness") and is run on its own.

Every run is guarded by `measure.py`: the process tree is killed when it exceeds the
memory available at start minus 2 GB, or after `MEASURE_BUDGET_S` seconds (default 600).
Throughput is pages actually served by the origin divided by wall time, the same count
for every tool, so a run cut short still reports a rate.

## Real-web simulation (`run_wild.sh`)

The clean sites above measure speed. Real sites waste a crawler's budget in other ways, so
`wild` (`src/bin/wild.rs`) serves sites whose problems are drawn at random from a seed:
tracking parameters and session IDs on links, print versions and `index.html` aliases,
relative links that grow forever (`more/more/…`), endless calendars and pagination,
sort/filter combinations, links to images and archives, robots.txt-disallowed areas,
random first-attempt 503s, and per-site rate limits that answer 429. The origin itself
classifies every request (first fetch of a content page, duplicate, trap, binary, robots
violation, 429/503) and reports per site at `/__report` (`/__log` lists every request).

```bash
cargo build --release --manifest-path benchmarks/crawl/Cargo.toml --bin wild
benchmarks/crawl/run_wild.sh            # fast and polite; SEED=n for another random web
```

Each site has 60 content pages and each tool may fetch 60 pages per site, so every wasted
request costs a content page. Coverage (content pages found / 60, higher is better), waste
share (lower is better), robots violations and 429s received (lower is better) and wall time
(lower is better) are summarised by `wild_score.py`.

## Tools and settings

| Tool | How it runs | Politeness settings |
|---|---|---|
| pagesift | `crawl_bench` example: `Scraper::crawl` per site, all concurrent | `default_interval`, `per_host_concurrency` |
| crw | `crw serve` (one process), one `/v1/crawl` job per site, polled | `per_host_min_interval_ms`, `per_host_max_concurrent`, robots on |
| Spider | `spider_crawl`: one `Website` per site, Markdown via `spider_transformations` | `with_delay`, `with_concurrency_limit` (at least 2: with a limit of 1 Spider visits only 5 links per site; with a delay it still sends one request at a time), robots on; anti-detection features off |
| Crawl4AI | `crawl4ai_crawl.py`: BFS deep crawl per site on one `AsyncWebCrawler`, HTTP and browser strategies; `crawl4ai_multi.py` splits the sites over N processes (N = min(cores, (available memory − 2 GB) / 2.5 GB)), since one Python process tops out near 15 pages/s whatever the concurrency | `mean_delay`, `max_range=0`, `semaphore_count`, `check_robots_txt=True` |

`measure.py` records wall time, CPU time and the peak RSS summed over the whole process
tree (so Crawl4AI's browser processes and crw's server are counted). Report every metric
with its direction: throughput higher is better; time, CPU and memory lower is better.

## Install

- Origin, wild origin and the Spider adapter: `cargo build --release --manifest-path benchmarks/crawl/Cargo.toml`
  (Spider 2.53.9, pinned in `Cargo.lock`); pagesift: `cargo build --release -p pagesift --example crawl_bench`.
- crw 0.39.0 (AGPL-3.0, evaluation only): `cargo install --locked --root benchmarks/data/tools crw-cli`.
- Scrapy 2.19.0 and Crawlee for Python 1.11.0: the shared `py` environment; Crawl4AI 0.9.5: `c4ai`
  ([Tool environments](../README.md#tool-environments)).
- Colly 2.3.0: a Go binary, `COLLY` defaults to `/home/yolow92/pagesift-bench-env/tools/bin/colly_crawl`
  (its source is not in this repository).
- Page bodies: `benchmarks/data/crawl-pages` (`PAGES_DIR`).

## Dry run (2026-10-27)

`benchmarks/crawl/run_all.sh smoke` (3 sites × 10 pages, every tool once) ran all nine entries —
pagesift, crw, Spider, Colly, Scrapy, Crawlee for Python, Crawl4AI HTTP, multi-process and browser —
and each fetched all 30 pages. Not run: `run_wild.sh`, whose Crawl4AI default
(`benchmarks/data/venvs/crawl4ai`) does not exist on the benchmark machine (pass
`C4A=benchmarks/data/fresh/tools/c4ai/bin/python`), and `engines.sh`.

`run_all.sh`, `run_wild.sh` and `engines.sh` used to run `pkill -x origin` (or `wild`) before each
scenario, which also stopped an origin another session had started. They now use
[`origin_ctl.sh`](origin_ctl.sh): each script stops only the origin it started (SIGTERM, then SIGKILL
after 5 s), and a leftover from an earlier run is stopped only if it listens on the script's port and
its executable and command line are this directory's origin binary with that port. `engines.sh` still
ends every Chrome, chrome-headless-shell, Lightpanda, Obscura and moli process of the user after each
engine (`cleanup`), including browsers that have nothing to do with the benchmark.
