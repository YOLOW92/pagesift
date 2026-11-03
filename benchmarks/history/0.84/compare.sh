#!/usr/bin/env bash
# 0.84 输出逐字一致：新旧两个 pagesift 二进制对同一批页面的 serve 结果逐条比对 SHA-256。
#
#   benchmarks/history/0.84/compare.sh <旧二进制> <新二进制>
#
# 语料复用 0.83 的四个集（label.py prepare）。原始输出写在 $MIX_TOOLS/runs-084/<时间>/，
# 汇总（逐条哈希和不一致的条目）写在 benchmarks/history/0.84/results/<时间>/。
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../../.." && pwd)
OLD=$(realpath "$1"); NEW=$(realpath "$2")
MIX=${MIX_TOOLS:-$HOME/mix-tools}
STAMP=$(date +%Y%m%d-%H%M%S)
RUN=$MIX/runs-084/$STAMP
OUT=$HERE/results/$STAMP
LOCK=${BENCH_LOCK:-$REPO/tasks/0.80/bench.lock}
PY=${PY:-$REPO/benchmarks/data/fresh/tools/py/bin/python}
mkdir -p "$RUN" "$OUT"

"$PY" -I "$REPO/benchmarks/history/0.83/label.py" prepare "$REPO" "$RUN"
N=$(wc -l < "$RUN/manifest.jsonl")
SITE_PORT=18484
python3 -I "$HERE/site.py" "$RUN/manifest.jsonl" $SITE_PORT &
SITE=$!
SERVE=
trap 'kill $SITE 2>/dev/null; [ -n "$SERVE" ] && kill -TERM $SERVE 2>/dev/null; rm -rf "$RUN/html" "$RUN"/data-*' EXIT
cat > "$RUN/config.toml" <<TOML
[fetch]
allow_private_network = true
[policy]
interval_ms = 0
per_host_concurrency = 16
[browser]
remote = "http://127.0.0.1:9"
TOML

snap() {  # 二进制 标签 端口
    "$1" serve --config "$RUN/config.toml" --no-cache --data-dir "$RUN/data-$2" --port "$3" > "$RUN/$2.serve.log" 2>&1 &
    SERVE=$!
    for _ in $(seq 100); do curl -sf "http://127.0.0.1:$3/healthz" >/dev/null && break; sleep 0.1; done
    curl -sf "http://127.0.0.1:$3/healthz" >/dev/null || { cat "$RUN/$2.serve.log"; exit 1; }
    python3 -I "$HERE/snapshot.py" "http://127.0.0.1:$3" "http://127.0.0.1:$SITE_PORT" "$N" "$RUN/$2.jsonl"
    kill -TERM $SERVE; wait $SERVE 2>/dev/null || true; SERVE=
}
snap "$OLD" old 18485
snap "$NEW" new 18486

python3 -I - "$RUN" "$OUT" <<'PY'
import hashlib, json, sys
run, out = sys.argv[1], sys.argv[2]
def load(tag):
    d = {}
    for l in open(f"{run}/{tag}.jsonl", encoding="utf-8"):
        r = json.loads(l)
        d[r["key"]] = hashlib.sha256(json.dumps(r["out"], sort_keys=True, ensure_ascii=False).encode()).hexdigest()
    return d
old, new = load("old"), load("new")
diff = sorted(k for k in old if old[k] != new.get(k))
with open(f"{out}/hashes.tsv", "w") as f:
    for k in sorted(old):
        f.write(f"{k}\t{old[k]}\t{new.get(k)}\n")
summary = {"items": len(old), "different": len(diff), "first_differences": diff[:50]}
json.dump(summary, open(f"{out}/summary.json", "w"), indent=2)
print(json.dumps(summary))
PY
echo "$OUT"
