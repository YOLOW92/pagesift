#!/usr/bin/env bash
# A3 定向比较：100 个需要 JavaScript 的实时网页，新旧两个 pagesift 二进制各抓一遍，逐页配对比较。
# 参照文字是 Playwright Firefox 在两次运行之前和之后各渲染一次，只算两次都出现的词（同 0.81 的 A3）。
#
#   benchmarks/history/0.87/a3.sh <旧二进制> <新二进制>
#
# 结果写到 benchmarks/history/0.87/results/a3-<时间>/：paired.md（配对差值与区间）、两边的 quality-a3.md。
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../../.." && pwd)
OLD=$(realpath "$1"); NEW=$(realpath "$2")
MIX=${MIX_TOOLS:-$HOME/mix-tools}
PY=${PY:-$REPO/benchmarks/data/fresh/tools/py/bin/python}
C4A=${C4A:-$REPO/benchmarks/data/fresh/tools/c4ai/bin/python}
export PLAYWRIGHT_BROWSERS_PATH=${PLAYWRIGHT_BROWSERS_PATH:-$MIX/ms-playwright}
STAMP=$(date +%Y%m%d-%H%M%S)
D=$MIX/runs-087/a3-$STAMP
OUT=$HERE/results/a3-$STAMP
mkdir -p "$D" "$OUT"
cd "$REPO"
log() { printf '%s %s\n' "$(date +%T)" "$*" >&2; }

"$PY" -I - "$REPO/benchmarks/mix/v2/samples/a3.tsv" "$D/manifest.txt" <<'PY'
import csv, sys
src, dst = sys.argv[1], sys.argv[2]
with open(src, encoding="utf-8") as f, open(dst, "w", encoding="utf-8") as g:
    for r in csv.DictReader(f, delimiter="\t"):
        g.write(f"{r['id']}\t{r['url']}\t{r['url']}\n")
PY
{ echo "date: $(date -Is)"; echo "host: $(uname -srm), $(nproc) CPU"; sha256sum "$OLD" "$NEW"; } > "$OUT/env.txt"

run() { # 标签 二进制
    log "a3: $1"
    MIX_PAGESIFT_BIN=$2 "$PY" -I benchmarks/mix/driver.py --tool pagesift-081-crawl \
        --manifest "$D/manifest.txt" --out "$D/runs-$1" --workers 4 --timeout 60 --mem-max 4096 \
        > "$D/driver-$1.json" 2> "$D/driver-$1.log"
}

log "a3: Firefox reference 1/2"
"$C4A" -I benchmarks/mix/v2/a3_reference.py "$D/manifest.txt" "$D/ref1" 6 > "$D/ref1.txt" 2>&1
run old "$OLD"
run new "$NEW"
log "a3: Firefox reference 2/2"
"$C4A" -I benchmarks/mix/v2/a3_reference.py "$D/manifest.txt" "$D/ref2" 6 > "$D/ref2.txt" 2>&1

for side in old new; do
    "$PY" -I benchmarks/mix/v2/score_a3.py "$D/ref1" "$D/ref2" "$D/runs-$side" pagesift-081-crawl > "$OUT/quality-$side.md"
done
"$PY" -I "$HERE/a3_paired.py" "$D/ref1" "$D/ref2" "$D/runs-old/pagesift-081-crawl" "$D/runs-new/pagesift-081-crawl" > "$OUT/paired.md"
cat "$OUT/paired.md"
echo "results: $OUT"
