#!/usr/bin/env bash
# 0.91 新旧输出比对（0.84 compare.sh 的可复现版本）：本地站点逐页提供 WebMainBench 545 页的 HTML，
# 新旧两个二进制的 serve 结果（0.84 snapshot.py：全部格式、每 10 页加 includeTags 和 engine: auto）逐条比对。
# 0.84 用的 1834 页里有不公开的快照，这里只用公开的 WebMainBench_545.jsonl（opendatalab/WebMainBench，
# 提交 5da0972e9b58d0c7891ae75053ced97c268f52e3）。结果只和同一台机器上的结果比。
#
#   benchmarks/history/0.91/compare_wmb.sh <WebMainBench_545.jsonl> <旧二进制> <新二进制>
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
SPEED=$HERE/../0.84
DATA=$(realpath "$1"); OLD=$(realpath "$2"); NEW=$(realpath "$3")
STAMP=$(date +%Y%m%d-%H%M%S)
RUN=$(mktemp -d)
OUT=$HERE/results/compare-$STAMP
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
SITE_PORT=18597
python3 -I "$SPEED/site.py" "$RUN/manifest.jsonl" $SITE_PORT &
SITE=$!
SERVE=
trap 'kill $SITE 2>/dev/null; [ -n "$SERVE" ] && kill -TERM $SERVE 2>/dev/null; rm -rf "$RUN"' EXIT
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
    python3 -I "$SPEED/snapshot.py" "http://127.0.0.1:$3" "http://127.0.0.1:$SITE_PORT" "$N" "$RUN/$2.jsonl"
    kill -TERM $SERVE; wait $SERVE 2>/dev/null || true; SERVE=
}
snap "$OLD" old 18598
snap "$NEW" new 18599
python3 -I - "$RUN" "$OUT" <<'PY'
import hashlib, json, sys
run, out = sys.argv[1], sys.argv[2]
def load(tag):
    d = {}
    for l in open(f"{run}/{tag}.jsonl", encoding="utf-8"):
        r = json.loads(l)
        d[r["key"]] = (hashlib.sha256(json.dumps(r["out"], sort_keys=True, ensure_ascii=False).encode()).hexdigest(), r["out"])
    return d
old, new = load("old"), load("new")
diff = sorted(k for k in old if old[k][0] != new.get(k, ("",))[0])
with open(f"{out}/hashes.tsv", "w") as f:
    for k in sorted(old):
        f.write(f"{k}\t{old[k][0]}\t{new.get(k, ('',))[0]}\n")
md = lambda o: ((o.get("data") or {}).get("markdown") or "")
md_diff = [k for k in diff if md(old[k][1]) != md(new[k][1])]
longer = [k for k in md_diff if len(md(new[k][1])) > len(md(old[k][1]))]
summary = {"items": len(old), "different": len(diff), "markdown_different": len(md_diff),
           "markdown_longer_in_new": len(longer), "first_differences": diff[:50]}
json.dump(summary, open(f"{out}/summary.json", "w"), indent=2)
print(json.dumps(summary))
PY
echo "$OUT"
