#!/bin/bash
cd ~/pagesift
OUT=bench/results/final/0.70/e1/C5
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
for spec in "8110 h1" "8111 h2" "8113 h4" "8114 h5"; do
  set -- $spec; PORT=$1; HOST=$2
  /usr/bin/time -f "WALL=%e RSS=%M" -o "$OUT/time_$HOST.txt" \
    timeout 480 ./target/release/pagesift --config "$OUT/e1c.toml" crawl "http://127.0.0.1:$PORT/" \
    --out "$OUT/$HOST" --limit 400 --depth 8 --entire-domain --sitemap include --engine http \
    > "$OUT/crawl_$HOST.log" 2>&1 &
done
wait
for spec in "8110 h1" "8111 h2" "8113 h4" "8114 h5"; do
  set -- $spec; HOST=$2
  printf "%-3s %-40s %s\n" "$HOST" "$(grep -o 'crawled [0-9]* pages, [0-9]* errors' "$OUT/crawl_$HOST.log"|tail -1)" "$(cat "$OUT/time_$HOST.txt" 2>/dev/null|tr '\n' ' ')"
done
echo "--- 错误码 ---"
grep -ohE "[A-Z][A-Z_]{3,}" "$OUT"/crawl_*.log | sort | uniq -c | sort -rn | head -8
echo E1C5_DONE
