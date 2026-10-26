# Head-to-head comparison

Every tool must see exactly the same HTML. The input set depends on pagesift itself
(`--auto` picks a browser-rendered snapshot when HTTP extraction is thin), so regenerate
it whenever extraction changes, then rerun every tool on it:

```bash
# 1. snapshots (polite fetch) and rendered snapshots
python3 -I benchmarks/common/fetch_snapshots.py benchmarks/data/1-0-0.csv benchmarks/data/snapshots
pagesift bench-render benchmarks/data

# 2. freeze the inputs for this round
rm -rf benchmarks/data/inputs
pagesift bench benchmarks/data --auto --emit-inputs benchmarks/data/inputs

# 3. run each tool on benchmarks/data/inputs/manifest.jsonl (adapters below), then score
pagesift bench benchmarks/data --auto --external benchmarks/data/compare/<tool>.jsonl
```

Adapters, all writing JSON lines `{id, markdown, ms}`:

| Tool | Command |
|---|---|
| Crawl4AI | `crawl4ai_adapter.py <manifest> <out_dir>` (writes raw and fit outputs) |
| trafilatura, MarkItDown | `py_adapter.py <tool> <manifest> <out>` |
| Readability + Turndown, Defuddle | `node js_adapter.mjs <tool> <node_modules dir> <manifest> <out>` |
| crw-extract, dom_smoothie | `rust-tools`: `pagesift-compare-rust <tool> <manifest> <out>` |
| pagesift Python package | `py_pagesift_bench.py <manifest> <out> [threads]` |

`parallel.sh <N> <manifest> <out> -- <adapter command>` shards the manifest over N processes.
Quality runs can run all tools at once; timing runs must run one tool at a time with the
machine otherwise idle, each tool using every core (16 processes or threads here).

crw-extract is AGPL-3.0: it is only used here for evaluation and is never a pagesift dependency.
