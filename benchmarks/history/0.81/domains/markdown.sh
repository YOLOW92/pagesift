#!/usr/bin/env bash
# HTML -> Markdown fidelity: 600 generated documents (3 seeds x 200).
# PageSift only; the scorer is the same GFM round-trip used in the old table.
set -euo pipefail
cd "$(dirname "$0")/.."
source ./lib.sh
cd "$REPO"
check_bin
stage_begin markdown

D=$RUN/markdown
mkdir -p "$D"

for seed in 1 2 3; do
    log "markdown: seed $seed"
    "$C4A" -I benchmarks/markdown/fidelity.py gen "$D/seed$seed/html" 200 "$seed" > "$D/gen-$seed.txt"
    "$EX/md_convert" "$D/seed$seed/html" "$D/seed$seed/pagesift" --full > "$D/t-$seed.txt" 2>&1
    "$C4A" -I benchmarks/markdown/fidelity.py score "$D/seed$seed/html" pagesift="$D/seed$seed/pagesift" > "$D/score-$seed.txt"
done

"$PY" -I - "$D" <<'PY' >> "$RUN/metrics.tsv"
import re, statistics, sys
from pathlib import Path

d = Path(sys.argv[1])
scores, times = [], []
for seed in (1, 2, 3):
    for line in (d / f"score-{seed}.txt").read_text().splitlines():
        p = line.split()
        if p and p[0] == "pagesift":
            scores.append(float(p[-1]))
    m = re.search(r"([0-9.]+)\s*ms/page", (d / f"t-{seed}.txt").read_text())
    if m:
        times.append(float(m.group(1)))
if scores:
    print(f"markdown.f1\t{statistics.mean(scores):.6f}")
    print(f"markdown.f1_min\t{min(scores):.6f}")
    print(f"markdown.docs\t{len(scores) * 200}")
if times:
    print(f"markdown.ms\t{statistics.mean(times):.6f}")
PY

"$PY" -I benchmarks/history/0.81/score/hash_outputs.py "$D" "$RUN/hashes" markdown >> "$RUN/metrics.tsv"
stage_end markdown
