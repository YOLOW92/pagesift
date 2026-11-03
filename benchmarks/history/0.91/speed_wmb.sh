#!/usr/bin/env bash
# 0.91 新旧二进制测速（0.84 speed.sh 的可复现版本）：本地站点逐页提供 WebMainBench 545 页的 HTML，
# 单请求延迟（并发 1）和并发吞吐（并发 16），新旧交替跑 ROUNDS 轮，每轮先整批预热一遍不计。
# 0.84 用的 1834 页里有不公开的快照，这里只用公开的 WebMainBench_545.jsonl（opendatalab/WebMainBench，
# 提交 5da0972e9b58d0c7891ae75053ced97c268f52e3）。结果只和同一台机器上的结果比。
#
#   benchmarks/history/0.91/speed_wmb.sh <WebMainBench_545.jsonl> <旧二进制> <新二进制>
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
SPEED=$HERE/../0.84
DATA=$(realpath "$1"); OLD=$(realpath "$2"); NEW=$(realpath "$3")
ROUNDS=${ROUNDS:-5}
STAMP=$(date +%Y%m%d-%H%M%S)
RUN=$(mktemp -d)
OUT=$HERE/results/speed-$STAMP
mkdir -p "$OUT" "$RUN/html"
python3 -I - "$DATA" "$RUN" <<'PY'
import json, sys
from pathlib import Path
run = Path(sys.argv[2])
with open(sys.argv[1], encoding="utf-8") as f, open(run / "manifest.jsonl", "w") as g:
    for line in f:
        r = json.loads(line)
        p = run / "html" / f"wmb-{r['track_id']}.html"
        p.write_text(r["html"], encoding="utf-8")
        g.write(json.dumps({"id": f"wmb-{r['track_id']}", "path": str(p)}) + "\n")
PY
N=$(wc -l < "$RUN/manifest.jsonl")
SITE_PORT=18587
python3 -I "$SPEED/site.py" "$RUN/manifest.jsonl" $SITE_PORT &
SITE=$!
SERVE=
trap 'kill $SITE 2>/dev/null; [ -n "$SERVE" ] && kill -TERM $SERVE 2>/dev/null; rm -rf "$RUN"' EXIT
cat > "$RUN/config.toml" <<TOML
[fetch]
allow_private_network = true
[policy]
interval_ms = 0
per_host_concurrency = 64
[browser]
remote = "http://127.0.0.1:9"
TOML
{
    echo "date: $(date -u +%FT%TZ)"
    echo "cpu: $(nproc) x $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 | xargs)"
    echo "kernel: $(uname -r)"
    echo "memory: $(free -g | awk '/Mem:/{print $2}') GiB"
    echo "pages: $N"
    echo "old: $("$OLD" --version) $(sha256sum "$OLD" | cut -c1-16)"
    echo "new: $("$NEW" --version) $(sha256sum "$NEW" | cut -c1-16)"
    echo "data: $(sha256sum "$DATA" | cut -c1-16)"
} > "$OUT/env.txt"

measure() {  # 二进制 标签 端口 轮次
    "$1" serve --config "$RUN/config.toml" --no-cache --data-dir "$RUN/data-$2" --port "$3" > "$RUN/$2.serve.log" 2>&1 &
    SERVE=$!
    for _ in $(seq 100); do curl -sf "http://127.0.0.1:$3/healthz" >/dev/null && break; sleep 0.1; done
    curl -sf "http://127.0.0.1:$3/healthz" >/dev/null || { cat "$RUN/$2.serve.log"; exit 1; }
    python3 -I "$SPEED/speed.py" "http://127.0.0.1:$3" "http://127.0.0.1:$SITE_PORT" "$N" 16 > /dev/null
    for c in 1 16; do
        python3 -I "$SPEED/speed.py" "http://127.0.0.1:$3" "http://127.0.0.1:$SITE_PORT" "$N" $c \
            | sed "s/^{/{\"bin\": \"$2\", \"round\": $4, /" >> "$OUT/speed.jsonl"
    done
    kill -TERM $SERVE; wait $SERVE 2>/dev/null || true; SERVE=
}
for r in $(seq "$ROUNDS"); do
    measure "$OLD" old 18588 "$r"
    measure "$NEW" new 18589 "$r"
done
python3 -I - "$OUT/speed.jsonl" <<'PY' | tee "$OUT/summary.txt"
import json, statistics, sys
rows = [json.loads(l) for l in open(sys.argv[1])]
for c, key in ((1, "p50_ms"), (1, "p95_ms"), (16, "pages_per_s")):
    for b in ("old", "new"):
        v = [r[key] for r in rows if r["bin"] == b and r["concurrency"] == c]
        print(f"concurrency {c} {b} {key}: median {statistics.median(v)} range {min(v)}–{max(v)} (n={len(v)})")
PY
echo "$OUT"
