#!/bin/bash
# E1-C 重跑：单请求超时 8 s，整轮 480 s 硬时限；未完成如实记为未完成。
cd ~/pagesift
OUT=bench/results/final/0.70/e1/C2
rm -rf "$OUT"; mkdir -p "$OUT"
cat > "$OUT/e1c.toml" <<'TOML'
[fetch]
timeout_secs = 8
max_body_mb = 15
allow_private_network = true
[policy]
interval_ms = 0
per_host_concurrency = 32
global_concurrency = 96
allow_crawl_ignore_robots = false
TOML
for spec in "8110 h1" "8111 h2" "8113 h4"; do
  set -- $spec; PORT=$1; HOST=$2
  timeout 480 ./target/release/pagesift --config "$OUT/e1c.toml" crawl "http://127.0.0.1:$PORT/" \
    --out "$OUT/$HOST" --limit 400 --depth 8 --entire-domain --sitemap include --engine http \
    > "$OUT/crawl_$HOST.log" 2>&1 &
done
wait
for spec in "8110 h1" "8111 h2" "8113 h4"; do
  set -- $spec; HOST=$2
  printf "%-3s %s\n" "$HOST" "$(grep -o 'crawled [0-9]* pages, [0-9]* errors' "$OUT/crawl_$HOST.log" | tail -1)"
done
echo "--- 错误码 ---"
grep -ohE "[A-Z][A-Z_]{3,}" "$OUT"/crawl_*.log | sort | uniq -c | sort -rn | head -10
echo E1C2_DONE
