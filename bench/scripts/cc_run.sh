#!/bin/bash
cd ~/pagesift
OUT=bench/results/final/0.70/common_crawl
BIN=bench/p4_2_webmainbench/target/release/pagesift-p4-2-webmainbench
echo "=== extract ==="
/usr/bin/time -v $BIN "$OUT/work/manifest.jsonl" "$OUT/work/pred.parts.jsonl" 192 \
  --hard-timeout-secs 300 --soft-warn-secs 60 > "$OUT/extract.log" 2>&1
echo "exit=$?"
grep -E "^loaded rows|^rows=|HANG" "$OUT/extract.log" | tail -5
grep -E "Elapsed|Maximum resident" "$OUT/extract.log"
wc -l "$OUT/work/pred.parts.jsonl"
echo "CC_RUN_DONE"
