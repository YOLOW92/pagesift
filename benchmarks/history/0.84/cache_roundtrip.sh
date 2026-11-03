#!/usr/bin/env bash
# 0.85 缓存格式的端到端验证：同一个二进制开缓存，同一批页面抓两遍。第二遍应全部命中缓存，
# 且结果与第一遍逐字一致（去掉 cacheState、cachedAt 两个本来就该变的字段）。
#
#   benchmarks/history/0.84/cache_roundtrip.sh <二进制>
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../../.." && pwd)
BIN=$(realpath "$1")
MIX=${MIX_TOOLS:-$HOME/mix-tools}
STAMP=$(date +%Y%m%d-%H%M%S)
RUN=$MIX/runs-084/cache-$STAMP
OUT=$HERE/results/cache-$STAMP
LOCK=${BENCH_LOCK:-$REPO/tasks/0.80/bench.lock}
PY=${PY:-$REPO/benchmarks/data/fresh/tools/py/bin/python}
mkdir -p "$RUN" "$OUT"

"$PY" -I "$REPO/benchmarks/history/0.83/label.py" prepare "$REPO" "$RUN"
N=$(wc -l < "$RUN/manifest.jsonl")
SITE_PORT=18490
python3 -I "$HERE/site.py" "$RUN/manifest.jsonl" $SITE_PORT &
SITE=$!
SERVE=
trap 'kill $SITE 2>/dev/null; [ -n "$SERVE" ] && kill -TERM $SERVE 2>/dev/null; rm -rf "$RUN/html" "$RUN/data" "$RUN"/pass*.jsonl' EXIT
cat > "$RUN/config.toml" <<TOML
[fetch]
allow_private_network = true
[policy]
interval_ms = 0
per_host_concurrency = 16
[browser]
remote = "http://127.0.0.1:9"
TOML

"$BIN" serve --config "$RUN/config.toml" --data-dir "$RUN/data" --port 18491 > "$RUN/serve.log" 2>&1 &
SERVE=$!
for _ in $(seq 100); do curl -sf http://127.0.0.1:18491/healthz >/dev/null && break; sleep 0.1; done
curl -sf http://127.0.0.1:18491/healthz >/dev/null || { cat "$RUN/serve.log"; exit 1; }
bash -c "
  python3 -I '$HERE/snapshot.py' http://127.0.0.1:18491 http://127.0.0.1:$SITE_PORT $N '$RUN/pass1.jsonl'
  python3 -I '$HERE/snapshot.py' http://127.0.0.1:18491 http://127.0.0.1:$SITE_PORT $N '$RUN/pass2.jsonl'"
kill -TERM $SERVE; wait $SERVE 2>/dev/null || true; SERVE=

python3 -I - "$RUN" "$OUT" <<'PY'
import hashlib, json, sys
run, out = sys.argv[1], sys.argv[2]
def load(tag):
    d, states = {}, {}
    for l in open(f"{run}/{tag}.jsonl", encoding="utf-8"):
        r = json.loads(l)
        meta = (r["out"].get("data") or {}).get("metadata") or {}
        states[r["key"]] = meta.pop("cacheState", None)
        meta.pop("cachedAt", None)
        d[r["key"]] = hashlib.sha256(json.dumps(r["out"], sort_keys=True, ensure_ascii=False).encode()).hexdigest()
    return d, states
a, sa = load("pass1")
b, sb = load("pass2")
diff = sorted(k for k in a if a[k] != b.get(k))
from collections import Counter
summary = {"items": len(a), "different": len(diff), "first_differences": diff[:50],
           "pass1_cache": dict(Counter(map(str, sa.values()))), "pass2_cache": dict(Counter(map(str, sb.values())))}
json.dump(summary, open(f"{out}/summary.json", "w"), indent=2)
print(json.dumps(summary))
PY
echo "$OUT"
