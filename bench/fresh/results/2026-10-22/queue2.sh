#!/usr/bin/env bash
# 续跑（2026-10-22 02:00）：抽取改成三个数据集一起排队之后接着跑，然后是要独占机器的阶段 B
cd /home/yolow92/pagesift-fresh
RES=$(bench/fresh/pinned.sh --reserved)
# 不再等：仍在运行的任务会被跳过
# while pgrep -f "js_adapter.mjs|py_adapter.py|crawl4ai_adapter.py|pagesift-compare-rust|extract_jsonl" >/dev/null; do sleep 5; done
echo "== extract resume $(date +%T)"
taskset -c $RES bench/fresh/run_extract.sh /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22 run > /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract.log 2>&1
echo "== extract done $(date +%T)"
echo "== phase B $(date +%T)"
bench/fresh/run_domains.sh /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22 wild > /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/wild.log 2>&1
echo "== wild done $(date +%T)"
(cd bench/crawl && PAGES_DIR=/home/yolow92/pagesift-fresh/bench/data/random/pages C4A=/home/yolow92/pagesift-fresh/bench/data/fresh/tools/c4ai/bin/python CRW=/home/yolow92/pagesift-fresh/bench/data/fresh/tools/crw/crw MEASURE_OUT=/home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/crawl.jsonl ./run_all.sh wide huge) > /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/crawl.log 2>&1
echo "== crawl done $(date +%T)"
(cd bench/crawl && PAGES_DIR=/home/yolow92/pagesift-fresh/bench/data/random/pages MEASURE_OUT=/home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/engines.jsonl ./engines.sh) > /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/engines.log 2>&1
echo "== engines done $(date +%T)"
echo "== all done $(date +%T)"
