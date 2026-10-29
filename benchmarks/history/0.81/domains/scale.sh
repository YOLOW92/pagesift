#!/usr/bin/env bash
# Scale smoke: 10M local requests through the 16-thread HTTP reactor runner.
# This is an appendix number: the 0.70 200M run used 192 vCPUs, so it is not
# comparable with that table.  The runner is rebuilt from the 0.81 tree.
set -euo pipefail
cd "$(dirname "$0")/.."
source ./lib.sh
cd "$REPO"
stage_begin scale

D=$RUN/scale
mkdir -p "$D"
TOTAL=${SCALE_TOTAL:-10000000}
PORT=19800
R=$SCALE_BIN/p2_http_reactor_runner
O=$SCALE_BIN/stream_origin
[ -x "$R" ] || die "missing $R (run prep.sh)"
[ -x "$O" ] || die "missing $O (run prep.sh)"
pgrep -f '[p]2_http_reactor_runner|[s]tream_origin' >/dev/null && die "another scale process is running"

cleanup() { kill "$OPID" 2>/dev/null || true; }
trap cleanup EXIT
ulimit -n 1048576 2>/dev/null || true
"$O" --bind 127.0.0.1:$PORT --total-pages "$TOTAL" > "$D/origin.log" 2>&1 &
OPID=$!
for _ in $(seq 1 80); do
    curl -fsS "http://127.0.0.1:$PORT/stats" >/dev/null 2>&1 && break
    sleep 0.25
done
curl -fsS "http://127.0.0.1:$PORT/stats" >/dev/null || die "scale origin did not start"

before=$(curl -fsS "http://127.0.0.1:$PORT/stats" | jq -r .requests_served)
start=$(date +%s%N)
set +e
timeout 900 "$R" --formal --mode full --split-pipeline --total-tasks "$TOTAL" \
    --telemetry-interval-ms 5000 --output "$D/scale-10m.json" > "$D/scale-10m.log" 2>&1
rc=$?
set -e
end=$(date +%s%N)
after=$(curl -fsS "http://127.0.0.1:$PORT/stats" | jq -r .requests_served)
(( rc == 0 )) || die "scale runner failed rc=$rc (see $D/scale-10m.log)"
wall=$(awk -v a="$start" -v b="$end" 'BEGIN{printf "%.3f",(b-a)/1e9}')
"$PY" -I - "$D/scale-10m.json" "$wall" "$before" "$after" "$TOTAL" <<'PY' > "$D/scale-10m.meta.json"
import json, sys
r = json.load(open(sys.argv[1]))
wall = float(sys.argv[2])
print(json.dumps({
    "total_tasks": int(sys.argv[5]),
    "wall_s": wall,
    "completed": r.get("completed"),
    "valid": r.get("valid"),
    "failed": r.get("failed"),
    "sustained_pages_per_s": (r.get("completed") or 0) / wall,
    "process_cpu_time_s": r.get("process_cpu_time_s"),
    "peak_rss_bytes": r.get("peak_rss_bytes"),
    "capacity_adjustments": len(r.get("capacity_adjustments") or []),
    "origin_requests": int(sys.argv[4]) - int(sys.argv[3]),
}, indent=1))
PY
"$PY" -I - "$D/scale-10m.meta.json" <<'PY' >> "$RUN/metrics.tsv"
import json, sys
r = json.load(open(sys.argv[1]))
for k in ("total_tasks", "wall_s", "completed", "valid", "failed", "sustained_pages_per_s", "peak_rss_bytes", "origin_requests"):
    print(f"scale.10m.{k}\t{r.get(k)}")
PY

"$PY" -I benchmarks/history/0.81/score/hash_outputs.py "$D" "$RUN/hashes" scale >> "$RUN/metrics.tsv"
stage_end scale
