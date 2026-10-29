#!/usr/bin/env bash
# PDF -> Markdown, olmOCR-bench born-digital subsets (1,269 pages).
# Speed: pdf_dir over exactly the five born-digital subsets.
# Quality: the B1 official score produced by v2_quality.sh (run here if absent).
set -euo pipefail
cd "$(dirname "$0")/.."
source ./lib.sh
cd "$REPO"
check_bin
stage_begin pdf

D=$RUN/pdf
SRC=$REPO/benchmarks/data/olmocr_1403/bench_data
mkdir -p "$D/data/pdfs"
for pair in "arxiv_math:arxiv_math" "headers_footers:headers_footers" "long_tiny_text:long_tiny_text" \
            "multi_column:multi_column" "table_tests:tables"; do
    name=${pair%%:*}; dir=${pair#*:}
    ln -sfn "$SRC/$name.jsonl" "$D/data/$name.jsonl"
    ln -sfn "$SRC/pdfs/$dir" "$D/data/pdfs/$dir"
done

log "pdf: conversion speed on 1,269 born-digital pages"
locked "$EX/pdf_dir" "$D/data/pdfs" "$D/data/pagesift" > "$D/pdf-dir.txt" 2>&1

# Reuse the B1 quality job if v2_quality.sh already ran; otherwise run and score it.
if [[ ! -f "$RUN/quality/report/quality-b1.json" ]]; then
    log "pdf: B1 quality not found, running PageSift 0.81 on the 1,269 pages"
    mkdir -p "$RUN/quality"
    MIX_PAGESIFT_BIN=$BIN MIX_OUT_ROOT=$RUN/quality "$PY" -I benchmarks/mix/run_batch.py b1:pagesift-081-file
    PAGESIFT_SCORER=$BIN "$PY" -I benchmarks/mix/score_quality.py b1 pagesift-081-file \
        --runs "$RUN/quality" --out "$RUN/quality/report"
fi

"$PY" -I - "$D" "$RUN/quality/report/quality-b1.json" <<'PY' >> "$RUN/metrics.tsv"
import json, re, sys
from pathlib import Path

d = Path(sys.argv[1])
txt = (d / "pdf-dir.txt").read_text()
m = re.search(r"pagesift:\s+([0-9]+) files,\s+([0-9]+) failed,\s+([0-9.]+) ms/file", txt)
if m:
    print(f"pdf.files\t{m.group(1)}")
    print(f"pdf.failed\t{m.group(2)}")
    print(f"pdf.ms\t{m.group(3)}")
q = Path(sys.argv[2])
if q.exists():
    rep = json.loads(q.read_text())
    r = rep.get("scores", {}).get("all", {})
    for set_ in ("olmOCR", "olmOCR 去掉 baseline"):
        try:
            v = r[set_]["pagesift-081-file"]["all_zero_fail"]
        except KeyError:
            continue
        key = "pdf.olmocr" if set_ == "olmOCR" else "pdf.nobase"
        print(f"{key}\t{v:.6f}")
    t = rep.get("tools", {}).get("pagesift-081-file", {})
    if t.get("completion") is not None:
        print(f"pdf.completion\t{t['completion']:.6f}")
PY

"$PY" -I benchmarks/history/0.81/score/hash_outputs.py "$D" "$RUN/hashes" pdf >> "$RUN/metrics.tsv"
stage_end pdf
