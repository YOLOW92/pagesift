#!/usr/bin/env bash
# 最终续跑：新代码编译 → 重新评分 → 三项整机测试
cd /home/yolow92/pagesift-fresh
export CARGO_BUILD_JOBS=8
cargo build --release -q -p pagesift --bins --examples && cargo build --release -q -p pagesift-content -p pagesift-extract -p pagesift-pdf --examples
echo "== built $(date +%T)"
target/release/examples/extract_raw /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/encoding/manifest.jsonl /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/encoding/pagesift.jsonl
bench/fresh/run_extract.sh /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22 run > /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract.log 2>&1
python3 -I bench/fresh/encoding.py score /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/encoding /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract pagesift trafilatura markitdown readability defuddle crw dom_smoothie crawl4ai > /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/encoding/score.txt 2>&1
echo "== scored $(date +%T)"
exec /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/queue3.sh
