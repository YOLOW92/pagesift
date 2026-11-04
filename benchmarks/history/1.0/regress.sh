#!/usr/bin/env bash
# 0.91.1 and the final build take turns on every local stage, older first (gate 2), then the stages only the final
# build runs: live-web crawl, A3 and B5 with OCR. Binaries are copied to the same disk as the inputs.
#   regress.sh <final examples/bin dir> <0.91.1 examples/bin dir> [stage ...]
set -uo pipefail
NEW_SRC=$(realpath "$1")
OLD_SRC=$(realpath "$2")
cd "$(dirname "$0")"
REPO=$(cd ../../.. && pwd)
MIX=${MIX_TOOLS:-$HOME/mix-tools}
STAMP=${RUN_ID:-$(date +%Y%m%d-%H%M%S)}
ROOT=$MIX/runs-100/$STAMP
NAMES="meta_eval md_convert chunk_spans qf_ranks pdf_dir office_bench crawl_bench extract_jsonl"
stage_bin() { # 目录 名字
    local d=$MIX/bin/$2
    mkdir -p "$d/examples"
    cp -f "$1/pagesift" "$d/" || { echo "没有 $1/pagesift" >&2; exit 1; }
    for e in $NAMES pdf_ocr_dir; do
        cp -f "$1/examples/$e" "$d/examples/" 2>/dev/null || [ "$e" = pdf_ocr_dir ] || { echo "没有示例 $e" >&2; exit 1; }
    done
    echo "$d"
}
NEW=$(stage_bin "$NEW_SRC" new) || exit 1
OLD=$(stage_bin "$OLD_SRC" old) || exit 1
shift 2
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
    case $s in
        wild|a3|b5ocr|a2fast) one new "$s" ;;
        *) if [ "${FIRST:-old}" = new ]; then one new "$s"; one old "$s"; else one old "$s"; one new "$s"; fi ;;
    esac
done
echo "$(date +%T) 结束 $ROOT"
