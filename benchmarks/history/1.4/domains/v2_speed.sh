#!/usr/bin/env bash
# 1.0 speed on the v2 workloads: A2 HTML (CLI + serve), A2 crawl replay,
# B1 PDF 10 % subset, B4 Office, at 1 and 16 workers.
#
#   ./domains/v2_speed.sh
#
# Runs benchmarks/mix/v2/run_speed_100.sh, which writes
# $RUN/v2speed/w<N>/<sub>/pagesift-100*/.
set -euo pipefail
cd "$(dirname "$0")/.."
source ./lib.sh
cd "$REPO"
check_bin
stage_begin v2speed

export MIX_PAGESIFT_BIN=$BIN
export MIX_OUT_ROOT=$RUN/v2speed
export MIX_TOOLS=$MIX
mkdir -p "$MIX/runs-v2" "$RUN/v2speed"

log "v2speed: B1 (10% model subset, 144 pages) & B4 (81 files) 1 worker"
rm -rf "$RUN/v2speed/w1" "$RUN/v2speed/w16"
MIX_OUT_ROOT="$RUN/v2speed/w1" MIX_SUBSET=model "$PY" -I benchmarks/mix/run_batch.py \
    b1:pagesift-100-file@1 b1:pagesift-100-serve-file@1
MIX_OUT_ROOT="$RUN/v2speed/w1" MIX_SUBSET= "$PY" -I benchmarks/mix/run_batch.py \
    b4:pagesift-100-file@1 b4:pagesift-100-serve-file@1

MIX_SPEED_ROOT="$RUN/v2speed" MIX_TOOLS=$MIX "$PY" -I benchmarks/mix/v2/peaks.py b1 b4 > "$RUN/v2speed/peaks.json"
export MIX_PER_WORKER_MB="$RUN/v2speed/peaks.json"

log "v2speed: B1 (144 pages) & B4 (81 files) 16 workers"
MIX_OUT_ROOT="$RUN/v2speed/w16" MIX_SUBSET=model "$PY" -I benchmarks/mix/run_batch.py \
    b1:pagesift-100-file@16 b1:pagesift-100-serve-file@16
MIX_OUT_ROOT="$RUN/v2speed/w16" MIX_SUBSET= "$PY" -I benchmarks/mix/run_batch.py \
    b4:pagesift-100-file@16 b4:pagesift-100-serve-file@16

"$PY" -I benchmarks/history/1.4/score/collect_speed.py "$RUN/v2speed" >> "$RUN/metrics.tsv"
"$PY" -I benchmarks/history/1.4/score/hash_outputs.py "$RUN/v2speed" "$RUN/hashes" v2speed >> "$RUN/metrics.tsv"
stage_end v2speed
