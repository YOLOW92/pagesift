#!/usr/bin/env bash
# Messy real-world sites: seeded wild origin, 40 sites x 60 pages, fast and polite.
# PageSift only; coverage/waste are scored by the origin, not by the tool.
set -euo pipefail
cd "$(dirname "$0")/.."
source ./lib.sh
cd "$REPO"
check_bin
stage_begin wild

D=$RUN/wild
mkdir -p "$D"
export PAGESIFT_BIN=$EX/crawl_bench
export TOOLS=pagesift
export WILD_HOSTS=${WILD_HOSTS:-40}
export WILD_PAGES=${WILD_PAGES:-60}
export SEED=${SEED:-1}
export OUT=$D/wild

log "wild: fast + polite (seed $SEED, ${WILD_HOSTS} sites)"
( cd benchmarks/crawl && ./run_wild.sh fast polite ) > "$D/run.log" 2>&1

"$PY" -I - "$OUT" <<'PY' >> "$RUN/metrics.tsv"
import json, os, sys
from pathlib import Path

d = Path(sys.argv[1])
meas = {}
for line in (d / "measure.jsonl").read_text().splitlines():
    r = json.loads(line)
    meas[r["label"]] = r
for scen in ("fast", "polite"):
    p = d / f"{scen}-pagesift.json"
    if not p.exists():
        continue
    sites = json.loads(p.read_text())
    req = sum(s["requests"] for s in sites)
    content = sum(s["unique_content"] for s in sites)
    kinds = {}
    for s in sites:
        for k, v in s["kinds"].items():
            kinds[k] = kinds.get(k, 0) + v
    useful = kinds.get("content", 0) + kinds.get("nav", 0)
    waste = req - useful - kinds.get("503", 0) - kinds.get("429", 0)
    limit = 60
    print(f"wild.{scen}.coverage\t{content / (len(sites) * limit):.6f}")
    print(f"wild.{scen}.waste\t{waste / max(req, 1):.6f}")
    print(f"wild.{scen}.requests\t{req}")
    print(f"wild.{scen}.content\t{content}")
    print(f"wild.{scen}.robots\t{kinds.get('robots', 0)}")
    print(f"wild.{scen}.429\t{kinds.get('429', 0)}")
    m = meas.get(f"wild-{scen}/pagesift", {})
    for key, out in (("wall_s", "wall"), ("peak_tree_pss_mb", "peak")):
        if m.get(key) is not None:
            print(f"wild.{scen}.{out}\t{m[key]}")
PY

"$PY" -I benchmarks/history/0.81/score/hash_outputs.py "$D" "$RUN/hashes" wild >> "$RUN/metrics.tsv"
stage_end wild
