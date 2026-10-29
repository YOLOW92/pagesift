#!/usr/bin/env bash
# B2 (71 random real PDFs), B3 (100 hard pdf.js PDFs) and B5 (134 scanned pages):
# speed at 1 and 16 workers.  Quality for the same sets is in v2_quality.sh.
set -euo pipefail
cd "$(dirname "$0")/.."
source ./lib.sh
cd "$REPO"
check_bin
stage_begin b3b5

D=$RUN/b3b5
mkdir -p "$D"
export MIX_PAGESIFT_BIN=$BIN

log "b3b5: 1 worker"
MIX_OUT_ROOT=$D/w1 "$PY" -I benchmarks/mix/run_batch.py \
    b2:pagesift-081-file@1 b3:pagesift-081-file@1 b5:pagesift-081-file@1

MIX_SPEED_ROOT=$D MIX_TOOLS=$MIX "$PY" -I benchmarks/mix/v2/peaks.py b2 b3 b5 > "$D/peaks.json"
export MIX_PER_WORKER_MB=$D/peaks.json

log "b3b5: 16 workers"
MIX_OUT_ROOT=$D/w16 "$PY" -I benchmarks/mix/run_batch.py \
    b2:pagesift-081-file@16 b3:pagesift-081-file@16 b5:pagesift-081-file@16

"$PY" -I - "$D" <<'PY' >> "$RUN/metrics.tsv"
import json, statistics, sys
from pathlib import Path

root = Path(sys.argv[1])
for sub in ("b2", "b3", "b5"):
    for w in (1, 16):
        d = root / f"w{w}" / sub / "pagesift-081-file"
        try:
            rows = [json.loads(l) for l in (d / "results.jsonl").read_text().splitlines() if l.strip()]
            summ = json.loads((d / "summary.json").read_text())
        except (OSError, ValueError):
            continue
        ms = sorted(r["ms"] for r in rows if r.get("status") == "ok" and r.get("ms") is not None)
        if ms:
            print(f"{sub}.p50.{w}w\t{statistics.median(ms):.6f}")
            print(f"{sub}.p95.{w}w\t{ms[min(len(ms)-1, int(.95*len(ms)))]:.6f}")
        if summ.get("wall_s"):
            print(f"{sub}.tput.{w}w\t{len(rows)/summ['wall_s']:.6f}")
        print(f"{sub}.ok.{w}w\t{sum(r.get('status')=='ok' for r in rows)}")
        peaks = [r.get("peak_mb") or 0 for r in rows]
        if peaks:
            print(f"{sub}.peak.{w}w\t{max(peaks):.1f}")
PY

"$PY" -I benchmarks/history/0.81/score/hash_outputs.py "$D" "$RUN/hashes" b3b5 >> "$RUN/metrics.tsv"
stage_end b3b5
