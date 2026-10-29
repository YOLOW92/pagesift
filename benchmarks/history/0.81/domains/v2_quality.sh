#!/usr/bin/env bash
# 0.81 quality regression on the frozen v2 samples.  Only PageSift is run;
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

log "quality jobs: A1/A2/A4/B1/B2/B3/B4/B5 (PageSift 0.81 only)"
MIX_OUT_ROOT=$Q "$PY" -I benchmarks/mix/run_batch.py \
    a1:pagesift-081 \
    a2:pagesift-081 a2:pagesift-081-crawl \
    a4:pagesift-081 \
    b1:pagesift-081-file \
    b2:pagesift-081-file \
    b3:pagesift-081-file \
    b4:pagesift-081-file \
    b5:pagesift-081-file

log "scoring quality"
R=$Q/report
mkdir -p "$R"
"$PY" -I benchmarks/mix/score_quality.py a1 pagesift-081 --runs "$Q" --out "$R"
"$PY" -I benchmarks/mix/score_quality.py a2 pagesift-081 pagesift-081-crawl --runs "$Q" --out "$R"
"$PY" -I benchmarks/mix/score_quality.py a4 pagesift-081 --runs "$Q" --out "$R"
"$PY" -I benchmarks/mix/score_quality.py b1 pagesift-081-file --runs "$Q" --out "$R"
"$PY" -I benchmarks/mix/score_quality.py b2 pagesift-081-file --runs "$Q" --out "$R"
"$PY" -I benchmarks/mix/score_quality.py b3 pagesift-081-file --runs "$Q" --out "$R"
"$PY" -I benchmarks/mix/score_quality.py b4 pagesift-081-file --runs "$Q" --out "$R"
"$PY" -I benchmarks/mix/score_quality.py b5 pagesift-081-file --runs "$Q" --out "$R"

"$PY" -I benchmarks/history/0.81/score/collect_quality.py "$Q/report" >> "$RUN/metrics.tsv"
"$PY" -I benchmarks/history/0.81/score/hash_outputs.py "$Q" "$RUN/hashes" quality >> "$RUN/metrics.tsv"

stage_end v2quality
