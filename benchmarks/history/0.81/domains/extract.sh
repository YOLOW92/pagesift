#!/usr/bin/env bash
# Firecrawl scrape-content-dataset-v1, 597 frozen HTML snapshots:
# quality (truth recall / noise leak / content score), single-thread timing,
# and full-core throughput.  Only PageSift is run.
set -euo pipefail
cd "$(dirname "$0")/.."
source ./lib.sh
cd "$REPO"
check_bin
stage_begin extract

D=$RUN/extract
mkdir -p "$D"

# The frozen Firecrawl manifest points at the old checkout (bench/data/...).
# All tools see the same bytes; only the path prefix is local.
"$PY" -I - "$REPO" "$D/manifest.jsonl" <<'PY'
import json, sys
repo, dst = sys.argv[1], sys.argv[2]
with open(f"{repo}/benchmarks/data/inputs/manifest.jsonl", encoding="utf-8") as f, open(dst, "w", encoding="utf-8") as g:
    for line in f:
        r = json.loads(line)
        r["path"] = f"{repo}/benchmarks/data/{r['path'].removeprefix('bench/data/')}"
        g.write(json.dumps(r, ensure_ascii=False) + "\n")
PY

log "extract: quality + full-core throughput"
locked "$BIN" bench "$REPO/benchmarks/data" --auto --jobs 16 --out "$D/bench.jsonl" > "$D/bench.txt" 2>&1

log "extract: single-thread timing"
locked "$EX/extract_jsonl" "$D/manifest.jsonl" "$D/one-thread.jsonl" > "$D/one-thread.txt" 2>&1

"$PY" -I - "$D" <<'PY' >> "$RUN/metrics.tsv"
import json, re, statistics, sys
from pathlib import Path

d = Path(sys.argv[1])
txt = (d / "bench.txt").read_text()
def grab(pat, cast=float):
    m = re.search(pat, txt)
    return cast(m.group(1)) if m else None
for key, val in [
    ("extract.recall", grab(r"mean truth recall:\s+([0-9.]+)")),
    ("extract.leak", grab(r"mean noise leak:\s+([0-9.]+)")),
    ("extract.score", grab(r"content score \(F1 of recall and 1 - leak\):\s+([0-9.]+)")),
    ("extract.p50", grab(r"p50\s+([0-9.]+)\s+ms")),
    ("extract.p95", grab(r"p95\s+([0-9.]+)\s+ms")),
    ("extract.tput16", grab(r"throughput:.*?([0-9]+)\s+pages/s", int)),
    ("extract.pages", grab(r"pages with truth phrases:\s+([0-9]+)", int)),
]:
    if val is not None:
        print(f"{key}\t{val}")

rows = [json.loads(l) for l in (d / "one-thread.jsonl").read_text().splitlines() if l.strip()]
ms = sorted(r["ms"] for r in rows if r.get("ms") is not None)
if ms:
    print(f"extract.1t.p50\t{statistics.median(ms):.6f}")
    print(f"extract.1t.p95\t{ms[min(len(ms)-1, int(.95*len(ms)))]:.6f}")
    print(f"extract.1t.mean\t{statistics.mean(ms):.6f}")
    print(f"extract.1t.pages\t{len(ms)}")
PY

"$PY" -I benchmarks/history/0.81/score/hash_outputs.py "$D" "$RUN/hashes" extract >> "$RUN/metrics.tsv"
stage_end extract
