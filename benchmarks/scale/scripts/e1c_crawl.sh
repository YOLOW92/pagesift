#!/bin/bash
cd ~/pagesift
OUT=benchmarks/history/0.70/results/e1/C
mkdir -p "$OUT"
CFG=benchmarks/scale/scripts/e1.toml
for spec in "8110 h1" "8111 h2" "8113 h4"; do
  set -- $spec; PORT=$1; HOST=$2
  /usr/bin/time -f "WALL=%e RSS=%M" -o "$OUT/time_$HOST.txt" \
    ./target/release/pagesift --config "$CFG" crawl "http://127.0.0.1:$PORT/" \
    --out "$OUT/$HOST" --limit 400 --depth 8 --entire-domain --sitemap include --engine http \
    > "$OUT/crawl_$HOST.log" 2>&1 &
done
wait
for spec in "8110 h1" "8111 h2" "8113 h4"; do
  set -- $spec; HOST=$2
  printf "%-3s %s | %s\n" "$HOST" "$(grep -o 'crawled [0-9]* pages, [0-9]* errors' "$OUT/crawl_$HOST.log" | tail -1)" "$(cat "$OUT/time_$HOST.txt" 2>/dev/null | tr '\n' ' ')"
done
echo "--- 错误类型分布 ---"
grep -ohE "[A-Z_]{4,}" "$OUT"/crawl_*.log | grep -vE "^(WALL|RSS)$" | sort | uniq -c | sort -rn | head -12
echo E1C_DONE
