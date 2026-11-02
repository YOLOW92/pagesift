#!/usr/bin/env bash
# 0.83 质量分校准：AEB、WCXB（test）、WebMainBench 545、Firecrawl 597 四个带标准答案的集，
# 用 pagesift-plan 的离线 example 提取特征、拟合模型，三折交叉验证的样本外预测按 DESIGN §9.3 判定校准是否达标。
#
#   benchmarks/history/0.83/run.sh
#
# 原始输出写在 $MIX_TOOLS/runs-083/<时间>/，汇总（report.json、weights.json、模型哈希）写在
# benchmarks/history/0.83/results/<时间>/。编译和提取都排 bench.lock。
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../../.." && pwd)
MIX=${MIX_TOOLS:-$HOME/mix-tools}
STAMP=$(date +%Y%m%d-%H%M%S)
RUN=$MIX/runs-083/$STAMP
OUT=$HERE/results/$STAMP
LOCK=${BENCH_LOCK:-$REPO/tasks/0.80/bench.lock}
PY=${PY:-$REPO/benchmarks/data/fresh/tools/py/bin/python}
mkdir -p "$RUN" "$OUT"
cd "$REPO"

flock "$LOCK" env CARGO_BUILD_JOBS="$("$REPO/scripts/jobs.sh")" \
    cargo build --release --locked -q -p pagesift-plan --example quality_data --example quality_fit
EX=$REPO/target/release/examples

"$PY" -I "$HERE/label.py" prepare "$REPO" "$RUN"
flock "$LOCK" "$EX/quality_data" "$RUN/manifest.jsonl" "$RUN/features.jsonl" "$OUT/weights.json"
"$PY" -I "$HERE/label.py" recall "$REPO" "$RUN"
"$EX/quality_fit" "$RUN/features.jsonl" "$RUN/recall.jsonl" "$RUN/quality-model.bin" "$OUT/report.json"
rm -r "${RUN:?}/html"
cp "$RUN/quality-model.bin" "$OUT/"
sha256sum "$RUN/manifest.jsonl" "$RUN/recall.jsonl" "$OUT/quality-model.bin" | sed "s|$RUN/||; s|$OUT/||" > "$OUT/SHA256SUMS"
git rev-parse HEAD > "$OUT/COMMIT"
git status --short --untracked-files=no | sed 's/^/dirty: /' >> "$OUT/COMMIT"
echo "$OUT"
