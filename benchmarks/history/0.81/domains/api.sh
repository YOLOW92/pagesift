#!/usr/bin/env bash
# Deployment / API appendix: binary size, cold start, Firecrawl v1/v2 and Jina
# Reader compatibility.  There is no same-method competitor number here.
set -euo pipefail
cd "$(dirname "$0")/.."
source ./lib.sh
cd "$REPO"
check_bin
stage_begin api

D=$RUN/api
mkdir -p "$D"
OUT=$REPO/benchmarks/production/api/baseline_results.json
BACKUP=$D/baseline_results.orig.json
HAD=0
if [[ -f "$OUT" ]]; then cp "$OUT" "$BACKUP"; HAD=1; fi
restore() { if [[ $HAD == 1 ]]; then cp "$BACKUP" "$OUT"; else rm -f "$OUT"; fi; }
trap restore EXIT

log "api: Firecrawl v1/v2 + Jina Reader compatibility"
( cd "$D" && "$REPO/benchmarks/data/six/venv/bin/python" -I "$REPO/benchmarks/production/api/benchmark.py" ) > "$D/api.txt" 2>&1
[[ -f "$OUT" ]] || die "API benchmark did not write $OUT"
cp "$OUT" "$D/api-results.json"

"$PY" -I - "$D" "$BIN" <<'PY' >> "$RUN/metrics.tsv"
import json, sys
from pathlib import Path

d, binp = Path(sys.argv[1]), Path(sys.argv[2])
r = json.loads((d / "api-results.json").read_text())
s = r.get("server", {})
for key, out in (("startup_ms", "api.startup_ms"), ("peak_rss_mb", "api.startup_rss_mb"),
                 ("binary_size_mb", "api.binary_size_mb")):
    if s.get(key) is not None:
        print(f"{out}\t{s[key]}")
print(f"api.binary_bytes\t{binp.stat().st_size}")
for v in ("firecrawl_v1", "firecrawl_v2", "jina_reader"):
    if r.get(v):
        print(f"api.{v}.keys\t{len(r[v])}")
PY

"$PY" -I benchmarks/history/0.81/score/hash_outputs.py "$D" "$RUN/hashes" api >> "$RUN/metrics.tsv"
stage_end api
