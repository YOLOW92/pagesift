#!/bin/bash
cd ~/pagesift
OUT=bench/results/final/0.70/webmainbench/v3
BIN=bench/p4_2_webmainbench/target/release/pagesift-p4-2-webmainbench
PY=bench/upstream/.venv/bin/python
mkdir -p "$OUT"
$BIN bench/data/p4_2_webmainbench/webmainbench.jsonl "$OUT/pred.parts.jsonl" 128 \
  --hard-timeout-secs 180 --soft-warn-secs 60 > "$OUT/extract.log" 2>&1
$PY bench/p4_2_webmainbench/finalize.py bench/data/p4_2_webmainbench/webmainbench.jsonl \
  "$OUT/pred.parts.jsonl" "$OUT/pred.jsonl" --summary "$OUT/finalize.json" >/dev/null
$PY bench/p4_2_webmainbench/score_full.py bench/data/p4_2_webmainbench/webmainbench.jsonl \
  "$OUT/pred.jsonl" bench/upstream/MinerU-HTML "$OUT/score" --workers 64 2>&1 | tail -10
echo D1_V3_DONE
