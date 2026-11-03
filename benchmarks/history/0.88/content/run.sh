#!/usr/bin/env bash
# 正文提取质量：A1 四套集（AEB、Firecrawl、WCXB、WebMainBench 各 150 页），新旧二进制逐页配对。
#
#   benchmarks/history/0.88/content/run.sh <旧二进制> <新二进制> <输出目录>
#
# 两边都用旧二进制的 `bench --external` 给 Firecrawl 集打分（评分器固定，只换被测的提取）。
# 质量评测不计时，不拿 bench.lock。结果：<输出目录>/{old,new}/report/quality-a1.{json,md}、paired.md。
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../../../.." && pwd)
PY=${PY:-$REPO/benchmarks/data/fresh/tools/py/bin/python}
OLD=$1 NEW=$2 OUT=$3
mkdir -p "$OUT"
for side in old new; do
    bin=$OLD; [ $side = new ] && bin=$NEW
    rm -rf "$OUT/$side"
    "$PY" -I "$HERE/run_a1.py" "$bin" "$OUT/$side" pagesift
    PAGESIFT_SCORER=$OLD MIX_SCORE_JOBS=${MIX_SCORE_JOBS:-4} \
        "$PY" -I "$REPO/benchmarks/mix/score_quality.py" a1 pagesift --runs "$OUT/$side" --out "$OUT/$side/report" >/dev/null
done
"$PY" -I "$HERE/paired.py" "$OUT/old/report/quality-a1.json" "$OUT/new/report/quality-a1.json" | tee "$OUT/paired.md"
