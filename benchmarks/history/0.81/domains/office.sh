#!/usr/bin/env bash
# Office / EPUB / CSV (B4, 81 files): in-process conversion time and per-file
# peak RSS.  Quality (structure kept) is the B4 score from v2_quality.sh.
set -euo pipefail
cd "$(dirname "$0")/.."
source ./lib.sh
cd "$REPO"
check_bin
stage_begin office

D=$RUN/office
mkdir -p "$D"
mapfile -t files < <(cut -f2 "$MIX/inputs/b4.manifest")

log "office: in-process time (3 runs per file)"
"$EX/office_bench" 3 "${files[@]}" > "$D/office-bench.tsv"

log "office: peak RSS, one process per file"
: > "$D/office-mem.tsv"
for f in "${files[@]}"; do
    PAGESIFT_CONFIG= /usr/bin/time -o "$D/.time" -f %M "$BIN" scrape "$f" >/dev/null 2>/dev/null || true
    printf '%s\t%s\n' "$(basename "$f")" "$(tail -1 "$D/.time")" >> "$D/office-mem.tsv"
done
rm -f "$D/.time"

if [[ ! -f "$RUN/quality/report/quality-b4.json" ]]; then
    log "office: B4 quality not found, running PageSift 0.81"
    mkdir -p "$RUN/quality"
    MIX_PAGESIFT_BIN=$BIN MIX_OUT_ROOT=$RUN/quality "$PY" -I benchmarks/mix/run_batch.py b4:pagesift-081-file
    PAGESIFT_SCORER=$BIN "$PY" -I benchmarks/mix/score_quality.py b4 pagesift-081-file \
        --runs "$RUN/quality" --out "$RUN/quality/report"
fi

"$PY" -I - "$D" "$RUN/quality/report/quality-b4.json" <<'PY' >> "$RUN/metrics.tsv"
import json, statistics, sys
from pathlib import Path

d = Path(sys.argv[1])
times = []
for line in (d / "office-bench.tsv").read_text().splitlines():
    p = line.split("\t")
    if len(p) >= 2 and p[1]:
        try:
            times.append(float(p[1]))
        except ValueError:
            pass
if times:
    total = sum(times)
    print(f"office.files\t{len(times)}")
    print(f"office.ms\t{statistics.mean(times):.6f}")
    print(f"office.files_s\t{1000.0 / statistics.mean(times):.6f}")
    print(f"office.total_ms\t{total:.3f}")
mem = []
for line in (d / "office-mem.tsv").read_text().splitlines():
    p = line.split("\t")
    if len(p) == 2 and p[1].isdigit():
        mem.append(int(p[1]) / 1024)
if mem:
    print(f"office.peak_mb\t{max(mem):.1f}")
    print(f"office.median_mb\t{statistics.median(mem):.1f}")
q = Path(sys.argv[2])
if q.exists():
    rep = json.loads(q.read_text())
    r = rep.get("scores", {}).get("all", {})
    for set_ in ("B4 holdout", "B4 dev"):
        try:
            v = r[set_]["pagesift-081-file"]["all_zero_fail"]
        except KeyError:
            continue
        print(f"office.{'holdout' if 'holdout' in set_ else 'dev'}\t{v:.6f}")
    t = rep.get("tools", {}).get("pagesift-081-file", {})
    if t.get("completion") is not None:
        print(f"office.completion\t{t['completion']:.6f}")
PY

"$PY" -I benchmarks/history/0.81/score/hash_outputs.py "$D" "$RUN/hashes" office >> "$RUN/metrics.tsv"
stage_end office
