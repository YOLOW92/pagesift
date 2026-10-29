#!/usr/bin/env bash
# 0.81 performance refresh: timed run (the 90-minute window) plus archive/report.
#
#   ./run.sh                 # all stages, in the planned order
#   ./run.sh v2quality pdf   # selected stages
#   ALLOW_ANY_VERSION=1 ./run.sh v2quality   # harness smoke with a non-0.81 binary
#
# The build and the frozen inputs must already exist (see prep.sh).  The run
# directory is $MIX/runs-081/<timestamp>; summaries, hashes and the scorecard
# are archived under benchmarks/history/0.81/results/<timestamp>/.
set -euo pipefail
cd "$(dirname "$0")"
source ./lib.sh

usage() {
    cat <<EOF
usage: $0 [stage ...]

stages:
  v2quality   A1/A2/A4/B1/B2/B3/B4/B5 quality, PageSift 0.81 only
  v2speed     A2 HTML/crawl, B1 PDF, B4 Office at 1 and 16 workers
  extract     Firecrawl 597 pages: quality, single-thread, 16-thread
  dates       htmldate 1,000 pages
  markdown    HTML -> Markdown, 600 generated documents
  rag         chunking + query filter, 472 questions
  pdf         olmOCR-bench born-digital 1,269 pages
  office      B4 81 files, time and memory
  b3b5        B2/B3/B5 speed at 1 and 16 workers
  crawl       wide + huge local crawl
  wild        seeded wild crawl, fast + polite
  a3          live JavaScript pages + Firefox reference
  browser     auto wide + Chrome smoke
  scale       10M local scale smoke
  api         deployment / API appendix
  hash        archive summaries and output hashes
  report      build the two-tier view and detailed report
  all         all of the above in this order
EOF
}

STAGES=("$@")
if [[ ${#STAGES[@]} -eq 0 || ${STAGES[0]} == all ]]; then
    STAGES=(v2quality v2speed extract dates markdown rag pdf office b3b5 crawl wild a3 browser scale api)
fi

check_bin
check_examples
need flock
need jq

mkdir -p "$RUN/manifests"
cp -f "$MIX/inputs"/*.manifest "$RUN/manifests/" 2>/dev/null || true
cp -f "$MIX/inputs"/manifests/*.tsv "$RUN/manifests/" 2>/dev/null || true
[[ -f "$MIX/inputs/prep.txt" ]] && cp -f "$MIX/inputs/prep.txt" "$RUN/prep.txt"

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
        v2quality) "$HIST_DIR/domains/v2_quality.sh" ;;
        v2speed)   "$HIST_DIR/domains/v2_speed.sh" ;;
        extract)   "$HIST_DIR/domains/extract.sh" ;;
        dates)     "$HIST_DIR/domains/dates.sh" ;;
        markdown)  "$HIST_DIR/domains/markdown.sh" ;;
        rag)       "$HIST_DIR/domains/rag.sh" ;;
        pdf)       "$HIST_DIR/domains/pdf.sh" ;;
        office)    "$HIST_DIR/domains/office.sh" ;;
        b3b5)      "$HIST_DIR/domains/b3b5.sh" ;;
        crawl)     "$HIST_DIR/domains/crawl.sh" ;;
        wild)      "$HIST_DIR/domains/wild.sh" ;;
        a3)        "$HIST_DIR/domains/a3.sh" ;;
        browser)   "$HIST_DIR/domains/browser.sh" ;;
        scale)     "$HIST_DIR/domains/scale.sh" ;;
        api)       "$HIST_DIR/domains/api.sh" ;;
        hash)      archive ;;
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

echo "finished	$(date -Is)" >> "$RUN/run.txt"
archive
if [[ " ${STAGES[*]} " == *" report "* ]]; then
    :
else
    log "run complete; build the view with: $0 report (or rerun with report)"
fi
log "raw: $RUN"
log "archive: $ARCHIVE"
