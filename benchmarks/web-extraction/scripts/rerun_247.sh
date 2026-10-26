#!/bin/bash
cd ~/pagesift
PY=bench/upstream/.venv/bin/python
BIN=bench/p4_2_webmainbench/target/release/pagesift-p4-2-webmainbench

echo "===== D4 WCXB ====="
OUT=bench/results/final/0.70/wcxb
$PY bench/fresh/wcxb.py prepare bench/data/wcxb "$OUT/work" all 2>&1 | tail -1
target/release/examples/extract_jsonl "$OUT/work/manifest.jsonl" "$OUT/pagesift.jsonl" 2>&1 | tail -1
$PY bench/fresh/wcxb.py score bench/data/wcxb "$OUT/work" all "$OUT/pagesift.jsonl" > "$OUT/score.txt" 2>&1
grep -E "F1:|Precision" "$OUT/score.txt" | head -2

echo "===== D7 Common Crawl ====="
OUT=bench/results/final/0.70/common_crawl
$BIN "$OUT/work/manifest.jsonl" "$OUT/work/pred.parts.jsonl" 192 \
  --hard-timeout-secs 300 --soft-warn-secs 60 > "$OUT/extract.log" 2>&1
grep -E "^rows=" "$OUT/extract.log"

echo "===== D2 WCEB ====="
OUTW=bench/results/final/0.70/wceb
$PY bench/p4_2_webmainbench/wceb_run.py bench/upstream/WCEB/datasets/combined "$OUTW/pred" $BIN 64 2>&1 | tail -3
$PY bench/p4_2_webmainbench/wceb_score.py bench/upstream/WCEB/datasets/combined "$OUTW/pred" "$OUTW/score" 64 2>&1 | tail -6
echo RERUN_DONE
