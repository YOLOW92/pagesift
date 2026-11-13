#!/usr/bin/env bash
# 1.0 performance refresh: timed run (the 90-minute window) plus archive/report.
#
#   ./run.sh                 # all stages, in the planned order
#   ./run.sh v2quality pdf   # selected stages
#   ALLOW_ANY_VERSION=1 ./run.sh v2quality   # harness smoke with a non-1.0 binary
#
# The build and the frozen inputs must already exist (see prep.sh).  The run
# directory is $MIX/runs-110/<timestamp>; summaries, hashes and the scorecard
# are archived under benchmarks/history/1.1/results/<timestamp>/.
set -euo pipefail
cd "$(dirname "$0")"
source ./lib.sh

usage() {
    cat <<EOF
usage: $0 [stage ...]

stages:
  v2quality   B1/B3/B4/B5 quality on local offline corpora, PageSift 1.4.0
  v2speed     B1 PDF and B4 Office speed at 1 and 16 workers
  markdown    HTML -> Markdown, 600 generated documents
  pdf         olmOCR-bench born-digital 1,269 pages
  office      B4 81 files (yolowdoc 0.1.1), time and memory
  b3b5        B3/B5 speed at 1 and 16 workers
  b5ocr       B5 with OCR: olmOCR score per configuration
  api         deployment / API appendix
  formats     format and SQLite smoke suite (373 code languages + UTF-8/UTF-16)
  yolow_deps  yolowsyntax 0.1.2 + yolowzstd 0.1.2 + yolowdoc 0.1.1 in-process benchmarks
  datagraph   DataGraph A/B overhead (AEB + WCXB) and multi-version crawl verification
  hash        archive summaries and output hashes
  report      build the scorecard view and detailed report
  all         all runnable stages in order
EOF
}

STAGES=("$@")
if [[ ${#STAGES[@]} -eq 0 || ${STAGES[0]} == all ]]; then
    STAGES=(v2quality v2speed markdown pdf office b3b5 b5ocr api formats yolow_deps datagraph)
fi

check_bin
check_examples
need flock
need jq

mkdir -p "$RUN/manifests"
cp -f "$MIX/inputs"/*.manifest "$RUN/manifests/" 2>/dev/null || true
cp -f "$MIX/inputs"/manifests/*.tsv "$RUN/manifests/" 2>/dev/null || true
[[ -f "$MIX/inputs/prep.txt" ]] && cp -f "$MIX/inputs/prep.txt" "$RUN/prep.txt"

# Keep the original timeline when `run.sh report` is run later against the
# same RUN_ROOT; only a fresh run creates run.txt.
if [[ ! -f "$RUN/run.txt" ]]; then
    {
        echo "run	$RUN"
        echo "archive	$ARCHIVE"
        echo "started	$(date -Is)"
        echo "pagesift	$PAGESIFT_VERSION"
        echo "binary	$BIN"
        echo "binary_sha256	$(sha256sum "$BIN" | cut -d' ' -f1)"
        echo "git_head	$(git -C "$REPO" rev-parse HEAD)"
        echo "git_status_begin"
        git -C "$REPO" status --short
    } > "$RUN/run.txt"
fi

archive() {
    mkdir -p "$ARCHIVE"
    cp -f "$RUN/run.txt" "$RUN/metrics.tsv" "$RUN/stage-times.tsv" "$ARCHIVE/" 2>/dev/null || true
    [[ -f "$RUN/prep.txt" ]] && cp -f "$RUN/prep.txt" "$ARCHIVE/"
    [[ -d "$RUN/hashes" ]] && cp -a "$RUN/hashes" "$ARCHIVE/"
    [[ -d "$RUN/manifests" ]] && cp -a "$RUN/manifests" "$ARCHIVE/"
    # Keep per-item results and all small summaries; leave huge rendered
    # outputs in $RUN (their SHA-256 is already archived).
    while IFS= read -r -d '' f; do
        rel=${f#"$RUN"/}
        mkdir -p "$ARCHIVE/$(dirname "$rel")"
        cp -f "$f" "$ARCHIVE/$rel"
    done < <(find "$RUN" -type f \( -name '*.json' -o -name '*.jsonl' -o -name '*.tsv' -o -name '*.txt' \) -size -8M -print0)
    find "$RUN" -maxdepth 3 -type f -name '*.md' -print0 | while IFS= read -r -d '' f; do
        rel=${f#"$RUN"/}
        mkdir -p "$ARCHIVE/$(dirname "$rel")"
        cp -f "$f" "$ARCHIVE/$rel"
    done
}

run_stage() {
    local s=$1
    case $s in
        v2quality)  "$HIST_DIR/domains/v2_quality.sh" ;;
        v2speed)    "$HIST_DIR/domains/v2_speed.sh" ;;
        markdown)   "$HIST_DIR/domains/markdown.sh" ;;
        pdf)        "$HIST_DIR/domains/pdf.sh" ;;
        office)     "$HIST_DIR/domains/office.sh" ;;
        b3b5)       "$HIST_DIR/domains/b3b5.sh" ;;
        b5ocr)      "$HIST_DIR/domains/b5ocr.sh" ;;
        api)        "$HIST_DIR/domains/api.sh" ;;
        formats)    "$HIST_DIR/domains/formats.sh" ;;
        yolow_deps) "$HIST_DIR/domains/yolow_deps.sh" ;;
        datagraph)  "$HIST_DIR/domains/datagraph.sh" ;;
        hash)       archive ;;
        report)
            archive
            "$PY" -I "$HIST_DIR/score/summarize.py" "$RUN" --out "$ARCHIVE"
            ;;
        *) usage; die "unknown stage: $s" ;;
    esac
}

for s in "${STAGES[@]}"; do
    case $s in
        hash|report) run_stage "$s" ;;
        *) run_stage "$s" ;;
    esac
done

grep -q '^finished' "$RUN/run.txt" || echo "finished	$(date -Is)" >> "$RUN/run.txt"
archive
if [[ " ${STAGES[*]} " == *" report "* ]]; then
    :
else
    log "run complete; build the view with: $0 report (or rerun with report)"
fi
log "raw: $RUN"
log "archive: $ARCHIVE"
