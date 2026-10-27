#!/bin/bash
cd ~/pagesift
OUT=benchmarks/history/0.70/results/crawlmaze
mkdir -p "$OUT"
./target/release/pagesift --config benchmarks/crawl/scripts/maze.toml crawl "http://127.0.0.1:8099/" \
  --out "$OUT/pagesift-http" --limit 600 --depth 12 --entire-domain \
  --sitemap include --engine http > "$OUT/crawl_http.log" 2>&1
echo "exit=$?"
tail -6 "$OUT/crawl_http.log"
wc -l "$OUT/pagesift-http/index.jsonl" 2>/dev/null
echo MAZE_CRAWL_DONE
