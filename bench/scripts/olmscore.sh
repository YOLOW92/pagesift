#!/bin/bash
cd ~/pagesift
OUT=bench/results/final/0.70/olmocr
bench/upstream/.venv/bin/python bench/p4_2_webmainbench/olmocr_score.py \
  bench/data/olmocr/bench/bench_data \
  "$OUT/md" "$OUT/score" 24 2>&1 | tail -30
echo "OLM_DONE"
