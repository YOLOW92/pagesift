#!/usr/bin/env bash
# 0.84 新旧二进制的定向测速（W4、W5 的本机代用）：同一本地站点、同一批 1834 页，
# 单请求延迟（并发 1）和并发吞吐（并发 16），新旧交替跑 ROUNDS 轮，减少机器状态漂移的影响。
#
#   benchmarks/history/0.84/speed.sh <旧二进制> <新二进制>
#   NEW_ONLY=1 benchmarks/history/0.84/speed.sh - <新二进制>    # 只测新版本
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../../.." && pwd)
NEW=$(realpath "$2"); [ -n "${NEW_ONLY:-}" ] || OLD=$(realpath "$1")
MIX=${MIX_TOOLS:-$HOME/mix-tools}
STAMP=$(date +%Y%m%d-%H%M%S)
RUN=$MIX/runs-084/speed-$STAMP
OUT=$HERE/results/speed-$STAMP
LOCK=${BENCH_LOCK:-$REPO/tasks/0.80/bench.lock}
PY=${PY:-$REPO/benchmarks/data/fresh/tools/py/bin/python}
ROUNDS=${ROUNDS:-3}
mkdir -p "$RUN" "$OUT"

"$PY" -I "$REPO/benchmarks/history/0.83/label.py" prepare "$REPO" "$RUN"
N=$(wc -l < "$RUN/manifest.jsonl")
SITE_PORT=18487
python3 -I "$HERE/site.py" "$RUN/manifest.jsonl" $SITE_PORT &
SITE=$!
SERVE=
trap 'kill $SITE 2>/dev/null; [ -n "$SERVE" ] && kill -TERM $SERVE 2>/dev/null; rm -rf "$RUN/html" "$RUN"/data-*' EXIT
cat > "$RUN/config.toml" <<TOML
[fetch]
allow_private_network = true
[policy]
interval_ms = 0
per_host_concurrency = 64
[browser]
remote = "http://127.0.0.1:9"
TOML

measure() {  # 二进制 标签 端口 轮次
    "$1" serve --config "$RUN/config.toml" --no-cache --data-dir "$RUN/data-$2" --port "$3" > "$RUN/$2.serve.log" 2>&1 &
    SERVE=$!
    for _ in $(seq 100); do curl -sf "http://127.0.0.1:$3/healthz" >/dev/null && break; sleep 0.1; done
    curl -sf "http://127.0.0.1:$3/healthz" >/dev/null || { cat "$RUN/$2.serve.log"; exit 1; }
    # 先整批跑一遍预热（连接、页缓存、学到的统计），不计
    python3 -I "$HERE/speed.py" "http://127.0.0.1:$3" "http://127.0.0.1:$SITE_PORT" "$N" 16 > /dev/null
    for c in 1 16; do
        python3 -I "$HERE/speed.py" "http://127.0.0.1:$3" "http://127.0.0.1:$SITE_PORT" "$N" $c \
            | sed "s/^{/{\"bin\": \"$2\", \"round\": $4, /" >> "$OUT/speed.jsonl"
    done
    kill -TERM $SERVE; wait $SERVE 2>/dev/null || true; SERVE=
}
# 先拿闸门（新的编译停在门外），再等正在跑的编译放开 bench.lock
exec 7>"$(dirname "$LOCK")/bench-gate.lock"; flock 7
exec 9>"$LOCK"; flock 9
for r in $(seq "$ROUNDS"); do
    [ -n "${NEW_ONLY:-}" ] || measure "$OLD" old 18488 "$r"
    measure "$NEW" new 18489 "$r"
done
flock -u 9
flock -u 7
python3 -I - "$OUT/speed.jsonl" <<'PY'
import json, statistics, sys
rows = [json.loads(l) for l in open(sys.argv[1])]
for c, key in ((1, "p50_ms"), (16, "pages_per_s")):
    for b in ("old", "new"):
        v = [r[key] for r in rows if r["bin"] == b and r["concurrency"] == c]
        if not v:
            continue
        print(f"concurrency {c} {b} {key}: median {statistics.median(v)} range {min(v)}–{max(v)}")
PY
echo "$OUT"
