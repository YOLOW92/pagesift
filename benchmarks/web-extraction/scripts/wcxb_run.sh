#!/bin/bash
cd ~/pagesift
export PATH=$HOME/.cargo/bin:$PATH
OUT=benchmarks/history/0.70/results/wcxb
mkdir -p "$OUT"
PY=benchmarks/common/vendor/.venv/bin/python

echo "=== prepare ==="
$PY benchmarks/common/fresh/wcxb.py prepare benchmarks/data/wcxb "$OUT/work" all 2>&1 | tail -3

echo "=== build extract_jsonl ==="
CARGO_TARGET_DIR=$HOME/pagesift/target cargo build --release -p pagesift-content --example extract_jsonl 2>&1 | tail -2

echo "=== extract ==="
time target/release/examples/extract_jsonl "$OUT/work/manifest.jsonl" "$OUT/pagesift.jsonl" 2>&1 | tail -5
wc -l "$OUT/pagesift.jsonl"

echo "=== score ==="
$PY benchmarks/common/fresh/wcxb.py score benchmarks/data/wcxb "$OUT/work" all "$OUT/pagesift.jsonl" 2>&1 | tail -40
echo "WCXB_DONE"
