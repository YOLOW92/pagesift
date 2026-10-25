#!/bin/bash
# E1-A 持续负载扫描：同一批唯一语料重复 10 轮，用于在 192 核上取得稳定高并发读数。
# 明确：unique samples = 13661 不变；这里 136,610 是 page-tasks（10 轮重复），
# 重复只用于测吞吐与扩展性，不当作新增唯一样本。
cd ~/pagesift
OUT=bench/results/final/0.70/e1/A
BIN=bench/p4_2_webmainbench/target/release/pagesift-p4-2-webmainbench
SRC=bench/results/final/0.70/e1/corpus/manifest.jsonl
REP=$OUT/manifest_x10.jsonl
if [ ! -f "$REP" ]; then
  for i in $(seq 1 10); do
    python3 - "$SRC" "$i" <<'PY'
import json,sys
src,rep=sys.argv[1],sys.argv[2]
for l in open(src,encoding="utf-8"):
    if not l.strip(): continue
    r=json.loads(l); r["track_id"]=r["track_id"]+"#"+rep
    sys.stdout.write(json.dumps(r,ensure_ascii=False)+"\n")
PY
  done > "$REP"
fi
wc -l "$REP"
echo "threads,total_tasks,wall_s,pages_per_s,peak_rss_kb" > "$OUT/sustain.csv"
for T in 32 64 128 192; do
  /usr/bin/time -f "%e %M" -o "$OUT/stime_$T.txt" \
    $BIN "$REP" "$OUT/spred_$T.jsonl" $T --hard-timeout-secs 600 --soft-warn-secs 120 \
    > "$OUT/srun_$T.log" 2>&1 &
  BP=$!
  : > "$OUT/scpu_$T.txt"
  while kill -0 $BP 2>/dev/null; do
    for t in /proc/$BP/task/*/stat; do [ -r "$t" ] && awk '{print $39}' "$t" 2>/dev/null; done >> "$OUT/scpu_$T.txt"
    sleep 0.1
  done
  wait $BP
  WALL=$(grep -o "wall_s=[0-9.]*" "$OUT/srun_$T.log" | tail -1 | cut -d= -f2)
  RSS=$(awk '{print $2}' "$OUT/stime_$T.txt" | tail -1)
  PPS=$(python3 -c "print('%.1f' % (136610/float('$WALL')))")
  echo "$T,136610,$WALL,$PPS,$RSS" >> "$OUT/sustain.csv"
  echo "T=$T wall=$WALL pps=$PPS rss=$RSS"
done
cat "$OUT/sustain.csv"
echo E1A_SUSTAIN_DONE
