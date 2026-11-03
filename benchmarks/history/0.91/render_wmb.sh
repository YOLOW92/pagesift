#!/usr/bin/env bash
# 0.91 渲染决策 A/B（WebMainBench 545，engine: auto，本机 Chromium）。要以非 root 用户运行（pagesift 不以 root 启动浏览器）。
#
#   benchmarks/history/0.91/render_wmb.sh <WebMainBench_545.jsonl> <旧二进制> <新二进制> <chrome 路径>
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
DATA=$(realpath "$1"); OLD=$(realpath "$2"); NEW=$(realpath "$3"); CHROME=$4
OUT=${OUT:-$HERE/results/render-$(date +%Y%m%d-%H%M%S)}
RUN=$(mktemp -d)
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
        g.write(json.dumps({"path": str(p)}) + "\n")
PY
SITE_PORT=18787
python3 -I "$HERE/../0.84/site.py" "$RUN/manifest.jsonl" $SITE_PORT &
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
path = "$CHROME"
TOML
for tag in old new; do
    bin=$OLD; [ $tag = new ] && bin=$NEW
    "$bin" serve --config "$RUN/config.toml" --no-cache --data-dir "$RUN/data-$tag" --port 18788 > "$RUN/$tag.serve.log" 2>&1 &
    SERVE=$!
    for _ in $(seq 100); do curl -sf http://127.0.0.1:18788/healthz >/dev/null && break; sleep 0.1; done
    python3 -I "$HERE/render_wmb.py" http://127.0.0.1:18788 "http://127.0.0.1:$SITE_PORT" "$DATA" "$OUT/$tag.jsonl"
    kill -TERM $SERVE; wait $SERVE 2>/dev/null || true; SERVE=
done
python3 -I "$HERE/render_summary.py" "$OUT" | tee "$OUT/summary.md"
