#!/usr/bin/env bash
# A3: 100 live pages that need JavaScript.  Reference text is Playwright
# Firefox rendered before and after the PageSift run; only words present in
# both renders count.  This is an appendix check, not a head-to-head row:
# the old Crawl4AI numbers are from a different live snapshot.
set -euo pipefail
cd "$(dirname "$0")/.."
source ./lib.sh
cd "$REPO"
check_bin
stage_begin a3

D=$RUN/a3
mkdir -p "$D"
"$PY" -I - "$REPO/benchmarks/mix/v2/samples/a3.tsv" "$D/manifest.txt" <<'PY'
import csv, sys
src, dst = sys.argv[1], sys.argv[2]
with open(src, encoding="utf-8") as f, open(dst, "w", encoding="utf-8") as g:
    for r in csv.DictReader(f, delimiter="\t"):
        g.write(f"{r['id']}\t{r['url']}\t{r['url']}\n")
PY

log "a3: Firefox reference 1/2"
PLAYWRIGHT_BROWSERS_PATH=$PLAYWRIGHT_BROWSERS_PATH "$C4A" -I benchmarks/mix/v2/a3_reference.py "$D/manifest.txt" "$D/ref1" 6 > "$D/ref1.txt" 2>&1

log "a3: PageSift 0.81 live crawl, 4 workers"
MIX_PAGESIFT_BIN=$BIN "$PY" -I benchmarks/mix/driver.py --tool pagesift-081-crawl \
    --manifest "$D/manifest.txt" --out "$D/runs" --workers 4 --timeout 60 --mem-max 4096 \
    > "$D/driver.json" 2> "$D/driver.log"

log "a3: Firefox reference 2/2"
PLAYWRIGHT_BROWSERS_PATH=$PLAYWRIGHT_BROWSERS_PATH "$C4A" -I benchmarks/mix/v2/a3_reference.py "$D/manifest.txt" "$D/ref2" 6 > "$D/ref2.txt" 2>&1

log "a3: scoring"
"$PY" -I benchmarks/mix/v2/score_a3.py "$D/ref1" "$D/ref2" "$D/runs" pagesift-081-crawl > "$D/quality-a3.md"

"$PY" -I - "$D" <<'PY' >> "$RUN/metrics.tsv"
import json, sys
from pathlib import Path

d = Path(sys.argv[1])
p = d / "runs" / "a3-summary.json"
if p.exists():
    s = json.loads(p.read_text())
    print(f"a3.pages\t{s['n']}")
    print(f"a3.ref_pages\t{s['n_ref']}")
    print(f"a3.common\t{s['n_common']}")
    t = s["tools"].get("pagesift-081-crawl", {})
    for key in ("completion", "coverage_all", "coverage_common", "p50", "p95", "peak"):
        if t.get(key) is not None:
            print(f"a3.{key}\t{t[key]}")
PY

"$PY" -I benchmarks/history/0.81/score/hash_outputs.py "$D" "$RUN/hashes" a3 >> "$RUN/metrics.tsv"
stage_end a3
