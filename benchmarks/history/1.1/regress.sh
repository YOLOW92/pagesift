#!/usr/bin/env bash
# The final build only: earlier versions and other tools keep their recorded numbers.
# Binaries are copied to the same disk as the inputs.
#   regress.sh <final bin dir> [stage ...]
set -uo pipefail
NEW_SRC=$(realpath "$1")
cd "$(dirname "$0")"
REPO=$(cd ../../.. && pwd)
MIX=${MIX_TOOLS:-$HOME/mix-tools}
STAMP=${RUN_ID:-$(date +%Y%m%d-%H%M%S)}
ROOT=$MIX/runs-110/$STAMP
NAMES="meta_eval md_convert chunk_spans qf_ranks pdf_dir office_bench crawl_bench extract_jsonl"
stage_bin() { # 目录 名字
    local d=$MIX/bin/$2
    mkdir -p "$d/examples"
    cp -f "$1/pagesift" "$d/" || { echo "没有 $1/pagesift" >&2; exit 1; }
    for e in $NAMES pdf_ocr_dir; do
        # 1.1 只重测改过的路径：没有编的示例跳过，用到它的阶段不跑
        cp -f "$1/examples/$e" "$d/examples/" 2>/dev/null || true
    done
    echo "$d"
}
NEW=$(stage_bin "$NEW_SRC" new-110) || exit 1
shift 1
STAGES=("$@")
[ ${#STAGES[@]} -gt 0 ] || STAGES=(v2quality v2speed extract dates markdown rag pdf office b3b5 crawl browser api)
one() { # old|new 阶段
    local who=$1 stage=$2 dir=$NEW scale=${SCALE_NEW:-}
    [ "$who" = old ] && { dir=$OLD; scale=${SCALE_OLD:-}; }
    echo "$(date +%T) $who $stage"
    ALLOW_ANY_VERSION=1 PAGESIFT_BIN=$dir/pagesift PAGESIFT_EXAMPLES=$dir/examples SCALE_BIN=$scale \
        RUN_ROOT=$ROOT/$who ARCHIVE_ROOT=$PWD/results/$STAMP/$who ./run.sh "$stage" > "$ROOT/$who.$stage.log" 2>&1 \
        || echo "失败：$who $stage（$ROOT/$who.$stage.log）"
}
mkdir -p "$ROOT"
for s in "${STAGES[@]}"; do
    one new "$s"
done
echo "$(date +%T) 结束 $ROOT"
