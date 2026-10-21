#!/usr/bin/env bash
# 内存耗尽中断后的续跑（2026-10-22 02:30）。整个脚本在内存上限 11 GB 的用户级隔离区里运行。
set -uo pipefail
cd /home/yolow92/pagesift-fresh
RES=$(bench/fresh/pinned.sh --reserved)
echo "== start $(date +%T)"; PIN_HT=1 bench/fresh/pinned.sh --cores
# 1. 随机语料上的两个 JS 工具：分段，用超线程模式，并发按内存限制
for t in readability:2 defuddle:10; do
    name=${t%%:*}; n=${t#*:}; P=/home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract/$name-parts; mkdir -p $P
    split -n l/$n -d -a 2 /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract/random-manifest.jsonl $P/part.
    for f in $P/part.??; do echo "bench/compare/js_sharded.sh $name bench/data/fresh/tools/node $f $f.out 2>$f.err"; done
done | PIN_HT=1 bench/fresh/pinned.sh
for name in readability defuddle; do
    ls /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract/$name-parts/part.??.out >/dev/null 2>&1 && cat /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract/$name-parts/part.??.out > /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract/random-$name.jsonl
    echo "$name: $(wc -l < /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract/random-$name.jsonl) 行"
done
echo "== js done $(date +%T)"
# 2. 修正过的 5 页由 8 个工具补跑，按页面编号换进完整结果
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
# 3. 评分
taskset -c $RES bench/fresh/run_extract.sh /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22 run > /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract.log 2>&1
echo "== extract scored $(date +%T)"
taskset -c $RES python3 -I bench/fresh/encoding.py score /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/encoding /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract pagesift trafilatura markitdown readability defuddle crw dom_smoothie crawl4ai > /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/encoding/score.txt 2>&1
echo "== encoding scored $(date +%T)"
# 4. 阶段 B：三项要独占整台机器的测试
exec /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/queue3.sh
