# Crawl showdown

Whole-site crawling under identical conditions: every tool crawls the same local sites,
follows the same links, obeys the same politeness rules, and is measured the same way.

```bash
# once: build the origin and the Spider adapter, and pagesift's driver
cargo build --release --manifest-path bench/crawl/Cargo.toml
cargo build --release -p pagesift --example crawl_bench
# crw (AGPL-3.0, evaluation only): cargo install --locked --root bench/data/tools crw-cli

bench/crawl/run_all.sh            # all scenarios; or: run_all.sh polite fast scale
```

## Sites

`origin` serves `s0.localhost` … `s{H-1}.localhost` from one port (`*.localhost`
resolves to `::1`, so every site is a separate host for per-host politeness). Each site
has P pages whose bodies are real pages from the extraction corpus
(`bench/data/crawl-pages`: corpus pages with no access barrier). Every original `href`
is renamed to `data-gone` (the name must not contain "href": some crawlers follow any
attribute containing it), and each page gets links to pages 3j+1…3j+3 and the start
page, so a breadth-first crawl from `/` reaches exactly P pages. A fixed per-response
delay simulates network latency. `robots.txt` allows everything.

## Scenarios

| Scenario | Sites × pages | Per-host interval | Per-host concurrency | Latency |
|---|---|---|---|---|
| polite | 100 × 50 | 1000 ms | 1 | 50 ms |
| fast | 20 × 100 | 0 | 4 | 20 ms |
| scale | 200 × 50 | 0 | 2 | 20 ms |

`polite` is the release-gate condition ("same politeness"): one request per second per
host, robots.txt checked. `fast` is crawling your own infrastructure. `scale` stresses
memory with many sites at once.

## Tools and settings

| Tool | How it runs | Politeness settings |
|---|---|---|
| pagesift | `crawl_bench` example: `Scraper::crawl` per site, all concurrent | `default_interval`, `per_host_concurrency` |
| crw | `crw serve` (one process), one `/v1/crawl` job per site, polled | `per_host_min_interval_ms`, `per_host_max_concurrent`, robots on |
| Spider | `spider_crawl`: one `Website` per site, Markdown via `spider_transformations` | `with_delay`, `with_concurrency_limit`, robots on; anti-detection features off |
| Crawl4AI | `crawl4ai_crawl.py`: BFS deep crawl per site on one `AsyncWebCrawler`, HTTP and browser strategies | `mean_delay`, `max_range=0`, `semaphore_count`, `check_robots_txt=True` |

`measure.py` records wall time, CPU time and the peak RSS summed over the whole process
tree (so Crawl4AI's browser processes and crw's server are counted). Report every metric
with its direction: throughput higher is better; time, CPU and memory lower is better.
