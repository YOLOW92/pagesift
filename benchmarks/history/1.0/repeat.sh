#!/usr/bin/env bash
# Repeat the stages that looked slower, taking turns on which build goes first, and print medians per metric.
#   repeat.sh <rounds> [stage ...]     binaries come from $MIX/bin/{new,old}
set -uo pipefail
cd "$(dirname "$0")"
REPO=$(cd ../../.. && pwd)
MIX=${MIX_TOOLS:-$HOME/mix-tools}
ROUNDS=$1; shift
STAGES=("$@")
[ ${#STAGES[@]} -gt 0 ] || STAGES=(crawl rag office)
ROOT=$MIX/runs-100/${RUN_ID:-repeat}
leg() { # 第几轮 old|new
    local i=$1 who=$2 dir=$MIX/bin/$2 s
    local r=$ROOT/$i/$who
    mkdir -p "$r"
    for s in "${STAGES[@]}"; do
        ALLOW_ANY_VERSION=1 PAGESIFT_BIN=$dir/pagesift PAGESIFT_EXAMPLES=$dir/examples RUN_ROOT=$r \
            ARCHIVE_ROOT=$r/archive ./run.sh "$s" > "$r/$s.log" 2>&1 || echo "失败：$i $who $s"
    done
    ( cd "$REPO" && MIX_PAGESIFT_BIN=$dir/pagesift MIX_OUT_ROOT=$r/serve MIX_PER_WORKER_MB=$MIX/runs-v2/peaks-100.json \
        python3 -I benchmarks/mix/run_batch.py a2:pagesift-100-serve@1 b4:pagesift-100-serve-file@1 a2:pagesift-100-serve-crawl@16 \
        > "$r/serve.log" 2>&1 ) || echo "失败：$i $who serve"
    for j in a2/pagesift-100-serve a2/pagesift-100-serve-crawl b4/pagesift-100-serve-file; do
        python3 -I - "$r/serve/$j" "${j//\//.}" >> "$r/metrics.tsv" <<'PY'
import json, statistics, sys
d, name = sys.argv[1:]
rows = [json.loads(l) for l in open(d + "/results.jsonl", encoding="utf-8") if l.strip()]
ms = sorted(r["ms"] for r in rows if r.get("status") == "ok" and r.get("ms") is not None)
wall = json.load(open(d + "/summary.json")).get("wall_s")
print(f"{name}.tput\t{len(ms) / wall}")
print(f"{name}.p50\t{statistics.median(ms)}")
PY
    done
}
for i in $(seq 1 "$ROUNDS"); do
    echo "$(date +%T) 第 $i 轮"
    if [ $((i % 2)) = 1 ]; then leg "$i" new; leg "$i" old; else leg "$i" old; leg "$i" new; fi
done
python3 -I - "$ROOT" <<'PY'
import statistics, sys
from pathlib import Path
root = Path(sys.argv[1])
vals = {}
for p in root.glob("*/*/metrics.tsv"):
    who = p.parent.name
    for line in p.read_text(encoding="utf-8").splitlines():
        k, _, v = line.partition("\t")
        try:
            vals.setdefault(k, {}).setdefault(who, []).append(float(v))
        except ValueError:
            pass
print("metric\told median\tnew median\tchange\told range\tnew range")
for k in sorted(vals):
    o, n = vals[k].get("old"), vals[k].get("new")
    if not o or not n or k.startswith("hashes."):
        continue
    mo, mn = statistics.median(o), statistics.median(n)
    print(f"{k}\t{mo:g}\t{mn:g}\t{(mn - mo) / mo if mo else 0:+.1%}\t{min(o):g}-{max(o):g}\t{min(n):g}-{max(n):g}")
PY
