#!/usr/bin/env bash
# 全量对比的执行顺序（2026-10-22）
#   0 独占：抽取输入准备（浏览器渲染、冻结输入、pagesift 全核吞吐）
#   A 共享核池：Markdown、RAG×2、日期、PDF、正文抽取的单线程计时同时跑，计时用物理核 0–6，
#     调度和评分限制在保留的物理核 7（CPU 7、15）
#   B 独占：爬取模拟、HTTP 爬取、浏览器内核
cd /home/yolow92/pagesift-fresh
RES=$(bench/fresh/pinned.sh --reserved)
echo "== prep $(date +%T)"; bench/fresh/pinned.sh --cores
bench/fresh/run_extract.sh /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22 prep > /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract-prep.log 2>&1
echo "== phase A $(date +%T)"
taskset -c $RES bash -c "bench/fresh/run_domains.sh /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22 markdown rag qasper dates pdf > /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/domains.log 2>&1 & bench/fresh/run_extract.sh /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22 run > /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract.log 2>&1 & wait"
echo "== phase B $(date +%T)"
bench/fresh/run_domains.sh /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22 wild > /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/wild.log 2>&1
(cd bench/crawl && PAGES_DIR=/home/yolow92/pagesift-fresh/bench/data/random/pages C4A=/home/yolow92/pagesift-fresh/bench/data/fresh/tools/c4ai/bin/python CRW=/home/yolow92/pagesift-fresh/bench/data/fresh/tools/crw/crw MEASURE_OUT=/home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/crawl.jsonl ./run_all.sh wide huge) > /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/crawl.log 2>&1
(cd bench/crawl && PAGES_DIR=/home/yolow92/pagesift-fresh/bench/data/random/pages MEASURE_OUT=/home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/engines.jsonl ./engines.sh) > /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/engines.log 2>&1
echo "== all done $(date +%T)"
