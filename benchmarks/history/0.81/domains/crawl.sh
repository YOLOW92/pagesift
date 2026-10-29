#!/usr/bin/env bash
# Whole-site crawling on the local multi-site origin: wide (200 x 100 pages)
# and huge (1,000 x 50 pages).  PageSift only; competitor numbers stay frozen.
set -euo pipefail
cd "$(dirname "$0")/.."
source ./lib.sh
cd "$REPO"
check_bin
stage_begin crawl

D=$RUN/crawl
mkdir -p "$D"
export PAGESIFT_BIN=$EX/crawl_bench
export MEASURE_OUT=$D/results.jsonl
export MEASURE_BUDGET_S=${MEASURE_BUDGET_S:-600}
export MEASURE_MAX_MB=${MEASURE_MAX_MB:-12288}

log "crawl: wide + huge"
( cd benchmarks/crawl && TOOLS=pagesift ./run_all.sh wide huge ) > "$D/run.log" 2>&1

"$PY" -I - "$D" <<'PY' >> "$RUN/metrics.tsv"
import json, sys
from pathlib import Path

d = Path(sys.argv[1])
for line in (d / "results.jsonl").read_text().splitlines():
    r = json.loads(line)
    name = r["label"].split("/", 1)[0]
    if r.get("pages_per_s") is not None:
        print(f"crawl.{name}.tput\t{r['pages_per_s']}")
    if r.get("peak_tree_pss_mb") is not None:
        print(f"crawl.{name}.peak\t{r['peak_tree_pss_mb']}")
    if r.get("wall_s") is not None:
        print(f"crawl.{name}.wall\t{r['wall_s']}")
    if r.get("served_pages") is not None:
        print(f"crawl.{name}.pages\t{r['served_pages']}")
    if r.get("killed"):
        print(f"crawl.{name}.killed\t{r['killed']}")
PY

"$PY" -I benchmarks/history/0.81/score/hash_outputs.py "$D" "$RUN/hashes" crawl >> "$RUN/metrics.tsv"
stage_end crawl
