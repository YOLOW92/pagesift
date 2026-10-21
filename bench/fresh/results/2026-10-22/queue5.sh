#!/usr/bin/env bash
# 修正 5 页 UTF-8 副本之后：等随机语料上的两个 JS 工具跑完，补跑这 5 页、换进结果，评分，然后阶段 B
cd /home/yolow92/pagesift-fresh
RES=$(bench/fresh/pinned.sh --reserved)
while pgrep -f "js_sharded.sh" >/dev/null; do sleep 15; done
echo "== js done $(date +%T)"
{
  echo "target/release/examples/extract_jsonl /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract/fix5/manifest.jsonl /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract/fix5/pagesift.jsonl"
  for t in trafilatura markitdown; do echo "bench/data/fresh/tools/py/bin/python -I bench/compare/py_adapter.py $t /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract/fix5/manifest.jsonl /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract/fix5/$t.jsonl 2>/dev/null"; done
  for t in readability defuddle; do echo "node bench/compare/js_adapter.mjs $t bench/data/fresh/tools/node /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract/fix5/manifest.jsonl /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract/fix5/$t.jsonl 2>/dev/null"; done
  for t in crw dom_smoothie; do echo "bench/compare/rust-tools/target/release/pagesift-compare-rust $t /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract/fix5/manifest.jsonl /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract/fix5/$t.jsonl 2>/dev/null"; done
  echo "CRAWL4AI_NO_CLOUD_NOTICE=1 bench/data/fresh/tools/c4ai/bin/python -I bench/compare/crawl4ai_adapter.py /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract/fix5/manifest.jsonl /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract/fix5/crawl4ai 2>/dev/null"
} | bench/fresh/pinned.sh
for t in pagesift trafilatura markitdown readability defuddle crw dom_smoothie; do python3 -I bench/fresh/splice.py /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract/random-$t.jsonl /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract/fix5/$t.jsonl; done
for v in raw fit; do python3 -I bench/fresh/splice.py /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract/random-crawl4ai/crawl4ai-$v.jsonl /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract/fix5/crawl4ai/crawl4ai-$v.jsonl; done
echo "== fix5 spliced $(date +%T)"
taskset -c $RES bench/fresh/run_extract.sh /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22 run > /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract.log 2>&1
echo "== extract scored $(date +%T)"
taskset -c $RES python3 -I bench/fresh/encoding.py score /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/encoding /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract pagesift trafilatura markitdown readability defuddle crw dom_smoothie crawl4ai > /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/encoding/score.txt 2>&1
echo "== encoding scored $(date +%T)"
exec /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/queue3.sh
