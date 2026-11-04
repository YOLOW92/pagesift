#!/usr/bin/env bash
# A2 replay on the same terms as tools that neither render nor rate-limit: --no-politeness and engine http.
set -euo pipefail
cd "$(dirname "$0")/.."
source ./lib.sh
cd "$REPO"
check_bin
stage_begin a2fast
D=$RUN/a2fast
export MIX_PAGESIFT_BIN=$BIN
for w in 1 16; do
    MIX_OUT_ROOT=$D/w$w "$PY" -I benchmarks/mix/run_batch.py a2:pagesift-100-fast-crawl@$w a2:pagesift-100-fast-serve-crawl@$w
done
"$PY" -I - "$D" >> "$RUN/metrics.tsv" <<'PY'
import json, statistics, sys
from pathlib import Path
root = Path(sys.argv[1])
for w in (1, 16):
    for kind, tool in (("cli", "pagesift-100-fast-crawl"), ("serve", "pagesift-100-fast-serve-crawl")):
        d = root / f"w{w}" / "a2" / tool
        rows = [json.loads(l) for l in (d / "results.jsonl").read_text().splitlines() if l.strip()]
        ms = [r["ms"] for r in rows if r.get("status") == "ok" and r.get("ms") is not None]
        wall = json.loads((d / "summary.json").read_text())["wall_s"]
        print(f"v2.a2fast.{kind}.{w}w.tput\t{len(rows) / wall:.6f}")
        print(f"v2.a2fast.{kind}.{w}w.p50\t{statistics.median(ms):.6f}")
        print(f"v2.a2fast.{kind}.{w}w.ok\t{len(ms) / len(rows):.6f}")
PY
stage_end a2fast
