#!/usr/bin/env bash
# Publication dates: htmldate's 1,000-page annotated set.  PageSift only.
# Quality is htmldate's own day-level scoring; speed is both the date search
# and the whole page conversion.
set -euo pipefail
cd "$(dirname "$0")/.."
source ./lib.sh
cd "$REPO"
check_bin
stage_begin dates

D=$RUN/dates
export PAGESIFT_META_EVAL=$EX/meta_eval
mkdir -p "$D"

log "dates: PageSift quality + date-search time"
"$PY" -I benchmarks/dates/dates.py --tools pagesift --dump "$D/dates.json" > "$D/dates.txt"

# Whole-page conversion time on the same 1,000 files, for the conservative
# 0.71 ms/page comparison in docs/benchmarks.md (the date search alone is ~0.06).
"$PY" -I - "$REPO" "$D/pages.jsonl" <<'PY'
import json, os, sys
repo, dst = sys.argv[1], sys.argv[2]
base = f"{repo}/benchmarks/data/fresh/data/htmldate/tests"
with open(dst, "w", encoding="utf-8") as f:
    for name in ("eval_mediacloud_2020.json", "eval_default.json"):
        for url, d in json.load(open(f"{base}/{name}", encoding="utf-8")).items():
            for sub in ("test_set", "cache", "eval"):
                p = f"{base}/{sub}/{d['file']}"
                if os.path.isfile(p):
                    f.write(json.dumps({"id": d["file"], "path": p, "url": url}, ensure_ascii=False) + "\n")
                    break
PY
locked "$EX/extract_jsonl" "$D/pages.jsonl" "$D/whole-page.jsonl" > "$D/whole-page.txt" 2>&1

"$PY" -I - "$D" <<'PY' >> "$RUN/metrics.tsv"
import json, re, statistics, sys
from pathlib import Path

d = Path(sys.argv[1])
txt = (d / "dates.txt").read_text()
for line in txt.splitlines():
    p = line.split()
    if len(p) == 7 and p[0] == "pagesift":
        half = p[1]
        keys = ("prec", "rec", "acc", "f1", "ms")
        for k, v in zip(keys, p[2:]):
            if half == "hold":
                print(f"dates.{k}_hold\t{v}")
            if half == "all":
                print(f"dates.{k}_all\t{v}")
rows = [json.loads(l) for l in (d / "whole-page.jsonl").read_text().splitlines() if l.strip()]
ms = [r["ms"] for r in rows if r.get("ms") is not None]
if ms:
    print(f"dates.ms_whole\t{statistics.mean(ms):.6f}")
    print(f"dates.pages\t{len(ms)}")
PY

"$PY" -I benchmarks/history/0.81/score/hash_outputs.py "$D" "$RUN/hashes" dates >> "$RUN/metrics.tsv"
stage_end dates
