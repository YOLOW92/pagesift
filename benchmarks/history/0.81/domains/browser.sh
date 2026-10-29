#!/usr/bin/env bash
# Browser rendering: the production auto chain on the full wide scenario, and
# a Chrome-only smoke run (3 x 10 pages) so the comparison has a baseline
# without spending 30+ minutes on Chrome wide.
set -euo pipefail
cd "$(dirname "$0")/.."
source ./lib.sh
cd "$REPO"
check_bin
stage_begin browser

D=$RUN/browser
mkdir -p "$D"
export PAGESIFT_BIN=$EX/crawl_bench

log "browser: auto wide"
( cd benchmarks/crawl && ENGINES=auto MEASURE_OUT=$D/auto.jsonl ./engines.sh wide ) > "$D/auto.log" 2>&1

log "browser: Chrome smoke"
( cd benchmarks/crawl && ENGINES=chrome MEASURE_OUT=$D/chrome-smoke.jsonl ./engines.sh smoke ) > "$D/chrome.log" 2>&1

"$PY" -I - "$D" <<'PY' >> "$RUN/metrics.tsv"
import json, sys
from pathlib import Path

d = Path(sys.argv[1])
for f, prefix in (("auto.jsonl", "browser.auto"), ("chrome-smoke.jsonl", "browser.chrome")):
    p = d / f
    if not p.exists():
        continue
    for line in p.read_text().splitlines():
        r = json.loads(line)
        label = r["label"]
        if label.endswith("/auto"):
            key = "browser.auto"
        elif label.endswith("/chrome"):
            key = "browser.chrome"
        else:
            continue
        if r.get("pages_per_s") is not None:
            print(f"{key}.tput\t{r['pages_per_s']}")
        if r.get("peak_tree_pss_mb") is not None:
            print(f"{key}.peak\t{r['peak_tree_pss_mb']}")
        if r.get("wall_s") is not None:
            print(f"{key}.wall\t{r['wall_s']}")
        if r.get("served_pages") is not None:
            print(f"{key}.pages\t{r['served_pages']}")
        if r.get("killed"):
            print(f"{key}.killed\t{r['killed']}")
PY

"$PY" -I benchmarks/history/0.81/score/hash_outputs.py "$D" "$RUN/hashes" browser >> "$RUN/metrics.tsv"
stage_end browser
