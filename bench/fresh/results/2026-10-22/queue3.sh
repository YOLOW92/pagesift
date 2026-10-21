#!/usr/bin/env bash
# 阶段 B（2026-10-22，脚本修复后）：等正文抽取跑完，再依次跑三项要独占整台机器的测试
cd /home/yolow92/pagesift-fresh
A=$PWD
# 由 queue4 调用时抽取已经跑完
while pgrep -f "bench/fresh/run_extract.sh" >/dev/null; do sleep 15; done
echo "== extract done $(date +%T)"
echo "== phase B $(date +%T)"
bench/fresh/run_domains.sh /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22 wild > /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/wild.log 2>&1
echo "== wild done $(date +%T)"
(cd bench/crawl && PAGES_DIR=$A/bench/data/random/utf8 C4A=$A/bench/data/fresh/tools/c4ai/bin/python CRW=$A/bench/data/fresh/tools/crw/crw MEASURE_OUT=/home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/crawl.jsonl ./run_all.sh wide huge) > /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/crawl.log 2>&1
echo "== crawl done $(date +%T)"
(cd bench/crawl && T=/home/yolow92/pagesift/bench/data/tools PAGES_DIR=$A/bench/data/random/utf8 MEASURE_OUT=/home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/engines.jsonl ./engines.sh wide huge) > /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/engines.log 2>&1
echo "== engines done $(date +%T)"
echo "== all done $(date +%T)"
