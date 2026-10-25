#!/bin/bash
# 本机 100M 亿级测试（16 核 / 15 GB）。口径与 P3 一致：--formal --mode full --split-pipeline。
set -u
cd ~/pagesift
R=bench/p0/target/release/p2_http_reactor_runner
O=bench/p0/target/release/stream_origin
OUT=bench/results/0.70_local_100m
mkdir -p bench/results
TOTAL=100000000

pkill -x stream_origin 2>/dev/null; sleep 1
ulimit -n 1048576
$O --bind 127.0.0.1:19800 --total-pages $TOTAL > /tmp/origin_local.log 2>&1 &
OPID=$!
for _ in $(seq 1 40); do curl -fsS http://127.0.0.1:19800/stats >/dev/null 2>&1 && break; sleep 0.25; done
curl -fsS http://127.0.0.1:19800/stats >/dev/null || { echo "origin failed"; tail -5 /tmp/origin_local.log; exit 1; }

BEFORE=$(curl -fsS http://127.0.0.1:19800/stats | jq -r .requests_served)
S=$(date +%s%N)
$R --formal --mode full --split-pipeline --total-tasks $TOTAL \
   --telemetry-interval-ms 5000 --output "$OUT.json" 2>&1 | tee "$OUT.log"
RC=${PIPESTATUS[0]}
E=$(date +%s%N)
AFTER=$(curl -fsS http://127.0.0.1:19800/stats | jq -r .requests_served)
kill $OPID 2>/dev/null
WALL=$(awk -v a=$S -v b=$E 'BEGIN{printf "%.3f",(b-a)/1e9}')
jq -n --argjson t $TOTAL --argjson w $WALL --argjson b "$BEFORE" --argjson a "$AFTER" \
   --slurpfile r "$OUT.json" \
   '{total_tasks:$t, wall_s:$w, completed:$r[0].completed, valid:$r[0].valid,
     failed:$r[0].failed, sustained_pages_per_s:($r[0].completed/$w),
     process_cpu_time_s:$r[0].process_cpu_time_s, peak_rss_bytes:$r[0].peak_rss_bytes,
     capacity_adjustments:($r[0].capacity_adjustments|length),
     samples:($r[0].samples|length), origin_requests:($a-$b)}' > "$OUT.meta.json"
cat "$OUT.meta.json"
echo "LOCAL_100M_DONE rc=$RC"
