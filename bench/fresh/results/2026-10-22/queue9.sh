#!/usr/bin/env bash
# 和爬取模拟同时进行：整站爬取（wide、huge），完成后浏览器内核（两者共用 18080 端口，只能先后）
cd /home/yolow92/pagesift-fresh/bench/crawl
echo "== crawl start $(date +%T)"
PAGES_DIR=/home/yolow92/pagesift-fresh/bench/data/random/utf8 C4A=/home/yolow92/pagesift-fresh/bench/data/fresh/tools/c4ai/bin/python CRW=/home/yolow92/pagesift-fresh/bench/data/fresh/tools/crw/crw MEASURE_OUT=/home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/crawl.jsonl ./run_all.sh wide huge > /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/crawl.log 2>&1
echo "== crawl done $(date +%T)"
T=/home/yolow92/pagesift/bench/data/tools PAGES_DIR=/home/yolow92/pagesift-fresh/bench/data/random/utf8 MEASURE_OUT=/home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/engines.jsonl ./engines.sh wide huge > /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/engines.log 2>&1
echo "== engines done $(date +%T)"
