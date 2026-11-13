#!/usr/bin/env bash
# 1.0 quality regression on the frozen v2 samples.  Only PageSift is run;
# every competitor number stays the 2026-10-27/28 recorded value.
#
#   ./domains/v2_quality.sh
#
# Outputs $RUN/quality/<sub>/<tool>/ and $RUN/quality/report/quality-<sub>.{json,md}.
set -euo pipefail
cd "$(dirname "$0")/.."
source ./lib.sh
cd "$REPO"
check_bin
stage_begin v2quality

Q=$RUN/quality
mkdir -p "$Q"
export MIX_PAGESIFT_BIN=$BIN
export PAGESIFT_SCORER=$BIN
export MIX_SCORE_JOBS=${MIX_SCORE_JOBS:-16}

log "quality jobs: B1/B3/B4/B5 (PageSift 1.4.0 on available local corpora)"
MIX_OUT_ROOT=$Q "$PY" -I benchmarks/mix/run_batch.py \
    b1:pagesift-100-file \
    b3:pagesift-100-file \
    b4:pagesift-100-file \
    b5:pagesift-100-file

log "scoring quality"
R=$Q/report
mkdir -p "$R"
"$PY" -I benchmarks/mix/score_quality.py b1 pagesift-100-file --runs "$Q" --out "$R"
"$PY" -I benchmarks/mix/score_quality.py b3 pagesift-100-file --runs "$Q" --out "$R"
"$PY" -I benchmarks/mix/score_quality.py b4 pagesift-100-file --runs "$Q" --out "$R"
"$PY" -I benchmarks/mix/score_quality.py b5 pagesift-100-file --runs "$Q" --out "$R"

"$PY" -I benchmarks/history/1.4/score/collect_quality.py "$Q/report" >> "$RUN/metrics.tsv"
"$PY" -I benchmarks/history/1.4/score/hash_outputs.py "$Q" "$RUN/hashes" quality >> "$RUN/metrics.tsv"

stage_end v2quality
