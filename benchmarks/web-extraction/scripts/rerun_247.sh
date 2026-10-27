#!/bin/bash
cd ~/pagesift
PY=benchmarks/common/vendor/.venv/bin/python
BIN=benchmarks/web-extraction/webmainbench/target/release/pagesift-p4-2-webmainbench

echo "===== D4 WCXB ====="
OUT=benchmarks/history/0.70/results/wcxb
$PY benchmarks/common/fresh/wcxb.py prepare benchmarks/data/wcxb "$OUT/work" all 2>&1 | tail -1
target/release/examples/extract_jsonl "$OUT/work/manifest.jsonl" "$OUT/pagesift.jsonl" 2>&1 | tail -1
$PY benchmarks/common/fresh/wcxb.py score benchmarks/data/wcxb "$OUT/work" all "$OUT/pagesift.jsonl" > "$OUT/score.txt" 2>&1
grep -E "F1:|Precision" "$OUT/score.txt" | head -2

echo "===== D7 Common Crawl ====="
OUT=benchmarks/history/0.70/results/common_crawl
$BIN "$OUT/work/manifest.jsonl" "$OUT/work/pred.parts.jsonl" 192 \
  --hard-timeout-secs 300 --soft-warn-secs 60 > "$OUT/extract.log" 2>&1
grep -E "^rows=" "$OUT/extract.log"

echo "===== D2 WCEB ====="
OUTW=benchmarks/history/0.70/results/wceb
$PY benchmarks/web-extraction/webmainbench/wceb_run.py benchmarks/common/vendor/WCEB/datasets/combined "$OUTW/pred" $BIN 64 2>&1 | tail -3
$PY benchmarks/web-extraction/webmainbench/wceb_score.py benchmarks/common/vendor/WCEB/datasets/combined "$OUTW/pred" "$OUTW/score" 64 2>&1 | tail -6
echo RERUN_DONE
