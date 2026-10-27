#!/bin/bash
cd ~/pagesift
OUT=benchmarks/history/0.70/results/olmocr
benchmarks/common/vendor/.venv/bin/python benchmarks/web-extraction/webmainbench/olmocr_score.py \
  benchmarks/data/olmocr/bench/bench_data \
  "$OUT/md" "$OUT/score" 24 2>&1 | tail -30
echo "OLM_DONE"
