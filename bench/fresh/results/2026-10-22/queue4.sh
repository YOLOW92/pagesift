#!/usr/bin/env bash
# 补跑随机语料上的两个 JS 工具（分批）并评分，然后阶段 B
cd /home/yolow92/pagesift-fresh
RES=$(bench/fresh/pinned.sh --reserved)
echo "== extract resume $(date +%T)"
taskset -c $RES bench/fresh/run_extract.sh /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22 run > /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/extract.log 2>&1
echo "== extract done $(date +%T)"
exec /home/yolow92/pagesift-fresh/bench/fresh/results/2026-10-22/queue3.sh
