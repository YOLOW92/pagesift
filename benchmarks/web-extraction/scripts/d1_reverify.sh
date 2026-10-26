#!/bin/bash
cd ~/pagesift
OUT=benchmarks/history/0.70/results/webmainbench/v3
BIN=benchmarks/web-extraction/webmainbench/target/release/pagesift-p4-2-webmainbench
PY=benchmarks/common/vendor/.venv/bin/python
mkdir -p "$OUT"
$BIN benchmarks/data/p4_2_webmainbench/webmainbench.jsonl "$OUT/pred.parts.jsonl" 128 \
  --hard-timeout-secs 180 --soft-warn-secs 60 > "$OUT/extract.log" 2>&1
$PY benchmarks/web-extraction/webmainbench/finalize.py benchmarks/data/p4_2_webmainbench/webmainbench.jsonl \
  "$OUT/pred.parts.jsonl" "$OUT/pred.jsonl" --summary "$OUT/finalize.json" >/dev/null
$PY benchmarks/web-extraction/webmainbench/score_full.py benchmarks/data/p4_2_webmainbench/webmainbench.jsonl \
  "$OUT/pred.jsonl" benchmarks/web-extraction/vendor/MinerU-HTML "$OUT/score" --workers 64 2>&1 | tail -10
echo D1_V3_DONE
