#!/bin/bash
# E1-A 并发扫描：固定语料，逐步加线程，记录吞吐与 NUMA 分布。
cd ~/pagesift
OUT=bench/results/final/0.70/e1/A
BIN=bench/p4_2_webmainbench/target/release/pagesift-p4-2-webmainbench
MAN=bench/results/final/0.70/e1/corpus/manifest.jsonl
mkdir -p "$OUT"
echo "threads,wall_s,pages_per_s,peak_rss_kb" > "$OUT/sweep.csv"
for T in 1 8 32 64 128 192; do
  echo "=== threads=$T ==="
  /usr/bin/time -f "%e %M" -o "$OUT/time_$T.txt" \
    $BIN "$MAN" "$OUT/pred_$T.jsonl" $T --hard-timeout-secs 600 --soft-warn-secs 120 \
    > "$OUT/run_$T.log" 2>&1 &
  BP=$!
  # NUMA 采样：记录每个线程当前所在 CPU
  : > "$OUT/cpu_$T.txt"
  while kill -0 $BP 2>/dev/null; do
    for t in /proc/$BP/task/*/stat; do [ -r "$t" ] && awk '{print $39}' "$t" 2>/dev/null; done >> "$OUT/cpu_$T.txt"
    sleep 0.2
  done
  wait $BP
  WALL=$(grep -o "wall_s=[0-9.]*" "$OUT/run_$T.log" | tail -1 | cut -d= -f2)
  RSS=$(awk '{print $2}' "$OUT/time_$T.txt" | tail -1)
  PPS=$(python3 -c "print('%.1f' % (13661/float('$WALL')))" 2>/dev/null)
  echo "$T,$WALL,$PPS,$RSS" >> "$OUT/sweep.csv"
  echo "  wall=${WALL}s  pps=$PPS  rss=${RSS}KB"
done
cat "$OUT/sweep.csv"
echo E1A_DONE
