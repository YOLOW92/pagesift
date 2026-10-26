#!/usr/bin/env bash
# PDF → Markdown 评测：olmOCR-bench 的五个电子版子集。用法：benchmarks/pdf/run.sh [工具 ...]
# 数据放在 benchmarks/data/fresh/data/olmocr-bench（jsonl + pdfs/），评分代码在 benchmarks/data/fresh/tools/olmocr-bench，
# Python 环境在 benchmarks/data/fresh/tools/py。结果写到 benchmarks/pdf/out/score-<工具>.txt。
set -euo pipefail
cd "$(dirname "$0")/../.."
DATA=$(realpath "${DATA:-benchmarks/data/fresh/data/olmocr-bench}")   # 也可以指向 dl_olmocr_1403.sh 下的 benchmarks/data/olmocr_1403/bench_data
OUT=benchmarks/pdf/out
mkdir -p $OUT
for f in $DATA/*.jsonl; do ln -sfn "$f" $OUT/; done
ln -sfn $DATA/pdfs $OUT/pdfs
TOOLS=${@:-pagesift pymupdf4llm pdfplumber pypdf markitdown}
# 转换：每个工具一个进程，各占一个物理核（benchmarks/common/fresh/pinned.sh），同时跑；单页耗时条件对所有工具相同
cargo build --release -q -p pagesift-pdf --example pdf_dir
rm -rf $OUT/pagesift
for t in $TOOLS; do
    if [ "$t" = pagesift ]; then
        echo "target/release/examples/pdf_dir $OUT/pdfs $OUT/pagesift > $OUT/timing-pagesift.txt"
    else
        echo "benchmarks/data/fresh/tools/py/bin/python -I benchmarks/pdf/py_tools.py $OUT/pdfs $OUT $t > $OUT/timing-$t.txt"
    fi
done | benchmarks/common/fresh/pinned.sh
cat $OUT/timing-*.txt
# 评分：各工具并行
for t in $TOOLS; do
    benchmarks/pdf/score.sh "$t" > $OUT/score-$t.txt &
done
wait
