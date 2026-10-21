#!/usr/bin/env bash
# 给一个工具的输出评分：bench/pdf/score.sh <工具名> [子集 jsonl ...]
# 输出在 bench/pdf/out/<工具名>/，评分用 olmOCR-bench 自己的 benchmark.py（经 bench_fast.py 加数学预筛，结果相同；只评电子版子集）。
set -euo pipefail
cd "$(dirname "$0")/../.."
OUT=$(realpath bench/pdf/out)
ROOT=$(pwd)
TOOL=$1; shift
BENCH=bench/data/fresh/tools/olmocr-bench
for j in ${@:-arxiv_math headers_footers long_tiny_text multi_column table_tests}; do
    (cd $BENCH && PYTHONPATH=. ../py/bin/python "$ROOT/bench/pdf/bench_fast.py" --dir "$OUT/$j.jsonl" --candidate "$TOOL" 2>/dev/null \
        | grep -A12 "Final Summary" | grep -E "Average Score|absent|present|order|table|math|baseline" | sed "s/^/$j: /")
done
