#!/bin/bash
# E1-B 复杂网络爬取：4 台受控源站并发爬取，核对"发现→允许→请求→解析→唯一内容→输出"。
cd ~/pagesift
OUT=bench/results/final/0.70/e1/B
mkdir -p "$OUT"
CFG=bench/scripts/e1.toml
LIMIT=${1:-400}
for spec in "8110 h1" "8111 h2" "8112 h3" "8113 h4"; do
  set -- $spec; PORT=$1; HOST=$2
  ./target/release/pagesift --config "$CFG" crawl "http://127.0.0.1:$PORT/" \
    --out "$OUT/$HOST" --limit $LIMIT --depth 8 --entire-domain --sitemap include --engine http \
    > "$OUT/crawl_$HOST.log" 2>&1 &
done
wait
for spec in "8110 h1" "8111 h2" "8112 h3" "8113 h4"; do
  set -- $spec; HOST=$2
  echo "$HOST: $(grep -o 'crawled [0-9]* pages, [0-9]* errors' "$OUT/crawl_$HOST.log" | tail -1)"
done
echo E1B_DONE
