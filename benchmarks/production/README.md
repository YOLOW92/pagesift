# Production workload and service API

| Path | What it is |
| :--- | :--- |
| [aiseek/](aiseek/) | AiSeek's production workload, replacing Crawl4AI ([docs/benchmarks.md §9](../../docs/benchmarks.md#9-a-production-workload-replacing-crawl4ai)) |
| [api/](api/) | 0.40 baseline for the Firecrawl v1/v2 and Jina Reader API surface |

## Rerun

```bash
cargo build --release -p pagesift
python3 -I benchmarks/production/api/benchmark.py      # rewrites api/baseline_results.json
```

`aiseek/` needs AiSeek's crawl bench in `~/crawl-bench` (Crawl4AI's `server.sh` and `urls_prod.csv`),
which is not in this repository; see `aiseek/bench.py`.

## Dry run (2026-10-27)

- `api/benchmark.py`: ran; all six endpoints (`/v1/scrape`, `/v1/map`, `/v1/crawl`, `/v2/scrape`,
  `/r/…`, SSE) answered 200. The first attempt failed: the script's `pagesift serve` used the default
  store directory, which a running `pagesift serve` on the same machine holds. The script now gives the
  server its own store directory.
- `aiseek/`: builds with `cd benchmarks/production/aiseek && cargo build --release` (its
  `.cargo/config.toml` puts the binary in `target/aiseek-bench/`, where `server.sh` looks; with
  `--manifest-path` from the repository root that config is not read and the binary lands in
  `aiseek/target/`). Not run: `~/crawl-bench` is not on the benchmark machine.
