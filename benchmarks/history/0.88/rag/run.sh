#!/usr/bin/env bash
# 0.88 RAG 分块判定：块预算 100/200/400 × 检索预算 250/500/1000，新旧 recursive 分块逐题配对。
#
#   benchmarks/history/0.88/rag/run.sh <旧 chunk_spans> <新 chunk_spans>
#
# 旧二进制：在提交 db34da1（0.81.5）上 `cargo build --profile release-fast -p pagesift-extract --example chunk_spans`；
# 新二进制：本工作区同样的命令。分块是确定性的，编译档不影响输出。
# 只评质量、不计时，不占 bench.lock（分块耗时只记在 time.txt 里备查，不作测速结论）。
# 结果写到 benchmarks/history/0.88/rag/results/<日期-时间>/：
#   chunks/{old,new}-<size>.json  块区间；cells-{all,tune,hold}.txt  九格判定；env.txt  二进制哈希与环境。
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../../../.." && pwd)
OLD=$(realpath "$1")
NEW=$(realpath "$2")
PY=${PY:-$REPO/benchmarks/data/fresh/tools/py/bin/python}   # bm25s、numpy
DATA=${DATA:-$REPO/benchmarks/data/fresh/data/chunkeval}
OUT=$HERE/results/$(date +%Y%m%d-%H%M%S)
mkdir -p "$OUT/chunks"
CORPORA=$(ls "$DATA"/corpora/*.md | tr '\n' ' ')

{
    echo "date: $(date -Is)"
    echo "host: $(uname -srm), $(nproc) CPU"
    sha256sum "$OLD" "$NEW"
    (cd "$DATA" && sha256sum corpora/*.md questions_df.csv)
} > "$OUT/env.txt"

for s in 100 200 400; do
    "$OLD" "{\"type\":\"recursive\",\"size\":$s}" "$OUT/chunks/old-$s.json" $CORPORA 2>> "$OUT/time.txt"
    "$NEW" "{\"type\":\"recursive\",\"size\":$s}" "$OUT/chunks/new-$s.json" $CORPORA 2>> "$OUT/time.txt"
done
(cd "$OUT/chunks" && sha256sum *.json) > "$OUT/chunks.sha256"
for h in all tune hold; do
    "$PY" -I "$HERE/cells.py" "$DATA" "$h" "$OUT/chunks/old" "$OUT/chunks/new" > "$OUT/cells-$h.txt"
done
cat "$OUT/cells-all.txt"
echo "results: $OUT"
