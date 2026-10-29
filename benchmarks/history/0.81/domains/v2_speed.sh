#!/usr/bin/env bash
# 0.81 speed on the v2 workloads: A2 HTML (CLI + serve), A2 crawl replay,
# B1 PDF 10 % subset, B4 Office, at 1 and 16 workers.
#
#   ./domains/v2_speed.sh
#
# Runs benchmarks/mix/v2/run_speed_081.sh, which writes
# $RUN/v2speed/w<N>/<sub>/pagesift-081*/.
set -euo pipefail
cd "$(dirname "$0")/.."
source ./lib.sh
cd "$REPO"
check_bin
stage_begin v2speed

export MIX_PAGESIFT_BIN=$BIN
export MIX_OUT_ROOT=$RUN/v2speed
export MIX_TOOLS=$MIX
mkdir -p "$MIX/runs-v2"
benchmarks/mix/v2/run_speed_081.sh

"$PY" -I benchmarks/history/0.81/score/collect_speed.py "$RUN/v2speed" >> "$RUN/metrics.tsv"
"$PY" -I benchmarks/history/0.81/score/hash_outputs.py "$RUN/v2speed" "$RUN/hashes" v2speed >> "$RUN/metrics.tsv"
stage_end v2speed
