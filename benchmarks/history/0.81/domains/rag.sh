#!/usr/bin/env bash
# Chunking and query filtering for RAG (Chroma chunking_evaluation, 472 questions).
# PageSift only; quality and time on the same frozen corpora.
set -euo pipefail
cd "$(dirname "$0")/.."
source ./lib.sh
cd "$REPO"
check_bin
stage_begin rag

D=$RUN/rag
DATA=$REPO/benchmarks/data/fresh/data/chunkeval
mkdir -p "$D/chunks"
CORPORA=$(ls "$DATA"/corpora/*.md | tr '\n' ' ')

log "rag: chunking strategies"
for spec in \
    'recursive:{"type":"recursive","size":200}' \
    'fixed:{"type":"fixed_tokens","size":200}' \
    'sliding:{"type":"sliding_window","window":200,"step":100}' \
    'paragraphs:{"type":"paragraphs"}' \
    'sentences:{"type":"sentences"}'; do
    name=${spec%%:*}
    json=${spec#*:}
    locked "$EX/chunk_spans" "$json" "$D/chunks/pagesift-$name.json" $CORPORA > "$D/t-pagesift-$name.txt" 2>&1
done

log "rag: query filter"
"$PY" -I benchmarks/rag/qf_prepare.py "$DATA" "$D/questions.jsonl"
locked "$EX/qf_ranks" "$DATA/corpora" "$D/questions.jsonl" "$D/qf-pagesift.json" > "$D/qf.txt" 2>&1

log "rag: scoring"
HALF=all "$PY" -I benchmarks/rag/eval.py "$DATA" "$D/chunks" > "$D/chunking-all.txt"
HALF=hold "$PY" -I benchmarks/rag/eval.py "$DATA" "$D/chunks" > "$D/chunking-hold.txt"
HALF=all "$PY" -I benchmarks/rag/qf_eval.py "$DATA/corpora" "$D/questions.jsonl" "$D/qf-pagesift.json" > "$D/qf-all.txt"
HALF=hold "$PY" -I benchmarks/rag/qf_eval.py "$DATA/corpora" "$D/questions.jsonl" "$D/qf-pagesift.json" > "$D/qf-hold.txt"

"$PY" -I - "$D" <<'PY' >> "$RUN/metrics.tsv"
import re, statistics, sys
from pathlib import Path

d = Path(sys.argv[1])

def table(path):
    lines = [l.split() for l in path.read_text().splitlines() if l.strip()]
    if not lines:
        return None, []
    return lines[0], lines[1:]

def find_row(rows, name):
    return next((r for r in rows if r and r[0] == name), None)

for half, path, prefix in (("all", d / "chunking-all.txt", "rag.chunk"), ("hold", d / "chunking-hold.txt", "rag.chunk_hold")):
    head, rows = table(path)
    r = find_row(rows, "pagesift-recursive")
    if not r:
        continue
    # method chunks words R@250 P@250 IoU@250 R@500 ...
    for label, idx in (("recall250", 3), ("recall500", 6), ("recall1000", 9), ("chunks", 1), ("words", 2)):
        print(f"{prefix}.{label}\t{r[idx]}")
for half, path, prefix in (("all", d / "qf-all.txt", "rag.qf"), ("hold", d / "qf-hold.txt", "rag.qf_hold")):
    head, rows = table(path)
    r = find_row(rows, "pagesift-default")
    if not r:
        continue
    # R@100 P@100 R@250 P@250 R@500 P@500 R(kept) words P(kept)
    print(f"{prefix}.recall_kept\t{r[-3]}")
    print(f"{prefix}.words\t{r[-2]}")
    print(f"{prefix}.precision_kept\t{r[-1]}")

m = re.search(r"chunking took ([0-9.]+) ms", (d / "t-pagesift-recursive.txt").read_text())
if m:
    print(f"rag.chunk.ms\t{m.group(1)}")
m = re.search(r"queryFilter: ([0-9.]+) ms/question", (d / "qf.txt").read_text())
if m:
    print(f"rag.qf.ms\t{m.group(1)}")
PY

"$PY" -I benchmarks/history/0.81/score/hash_outputs.py "$D" "$RUN/hashes" rag >> "$RUN/metrics.tsv"
stage_end rag
