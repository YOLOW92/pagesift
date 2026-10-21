#!/usr/bin/env bash
# PDF → Markdown 评测：olmOCR-bench 的五个电子版子集。用法：bench/pdf/run.sh [工具 ...]
# 数据放在 bench/data/fresh/data/olmocr-bench（jsonl + pdfs/），评分代码在 bench/data/fresh/tools/olmocr-bench，
# Python 环境在 bench/data/fresh/tools/py。结果写到 bench/pdf/out/score-<工具>.txt。
set -euo pipefail
cd "$(dirname "$0")/../.."
DATA=$(realpath bench/data/fresh/data/olmocr-bench)
OUT=bench/pdf/out
mkdir -p $OUT
for f in $DATA/*.jsonl; do ln -sfn "$f" $OUT/; done
ln -sfn $DATA/pdfs $OUT/pdfs
TOOLS=${@:-pagesift pymupdf4llm pdfplumber pypdf markitdown}
PY=()
for t in $TOOLS; do
    if [ "$t" = pagesift ]; then
        cargo build --release -q -p pagesift-pdf --example pdf_dir
        rm -rf $OUT/pagesift
        target/release/examples/pdf_dir $OUT/pdfs $OUT/pagesift | tee $OUT/timing-pagesift.txt
    else
        PY+=("$t")
    fi
done
if [ ${#PY[@]} -gt 0 ]; then
    bench/data/fresh/tools/py/bin/python -I bench/pdf/py_tools.py $OUT/pdfs $OUT "${PY[@]}" | tee $OUT/py_timing.txt
fi
for t in $TOOLS; do
    bench/pdf/score.sh "$t" > $OUT/score-$t.txt
done
