#!/usr/bin/env bash
# 分块和检索评测（数据：Chroma chunking_evaluation，5 份语料、472 个问题，下载到 bench/data/fresh/data/chunkeval）。
#   1. 分块：每种分块器按约 200 词切块，同一个 bm25s 在全部块上检索，按词数预算比召回、精确率、IoU（eval.py）；
#   2. queryFilter：每个问题在它所属的文档里筛选，比"原样输出"的召回和输出词数，以及同预算下的排序质量（qf_eval.py）。
# 结果写到 $OUT（默认 bench/rag/out）；HALF=tune/hold/all 选问题子集（默认三个都跑）。
set -euo pipefail
cd "$(dirname "$0")/../.."
OUT=${OUT:-bench/rag/out}
DATA=${DATA:-bench/data/fresh/data/chunkeval}
PY=${PY:-bench/data/fresh/tools/py/bin/python}     # bm25s、langchain-text-splitters、chonkie、semchunk
C4A=${C4A:-bench/data/venvs/crawl4ai/bin/python}   # Crawl4AI
SIZE=${SIZE:-200}
rm -rf "$OUT"; mkdir -p "$OUT/chunks"
cargo build --release -q -p pagesift-extract --example chunk_spans --example qf_ranks
CORPORA=$(ls "$DATA"/corpora/*.md)
exec 2> >(tee "$OUT/timing.txt" >&2)   # 各工具的耗时打在标准错误上，一并存下
for spec in "recursive:{\"type\":\"recursive\",\"size\":$SIZE}" "fixed:{\"type\":\"fixed_tokens\",\"size\":$SIZE}" \
            "sliding:{\"type\":\"sliding_window\",\"window\":$SIZE,\"step\":$((SIZE / 2))}" \
            "paragraphs:{\"type\":\"paragraphs\"}" "sentences:{\"type\":\"sentences\"}"; do
    target/release/examples/chunk_spans "${spec#*:}" "$OUT/chunks/pagesift-${spec%%:*}.json" $CORPORA 2>&1 | sed "s/^/pagesift-${spec%%:*}: /"
done
"$PY" -I bench/rag/chunkers.py "$OUT/chunks" "$SIZE" langchain-recursive chonkie-recursive chonkie-sentence chonkie-token semchunk -- $CORPORA
"$C4A" -I bench/rag/chunkers.py "$OUT/chunks" "$SIZE" crawl4ai-fixed crawl4ai-sliding crawl4ai-regex -- $CORPORA
for h in all tune hold; do
    echo "== chunking ($h)"; HALF=$h "$PY" -I bench/rag/eval.py "$DATA" "$OUT/chunks"
done | tee "$OUT/chunking.txt"
python3 -I bench/rag/qf_prepare.py "$DATA" "$OUT/questions.jsonl"
target/release/examples/qf_ranks "$DATA/corpora" "$OUT/questions.jsonl" "$OUT/qf-pagesift.json"
"$C4A" -I bench/rag/qf_crawl4ai.py "$DATA/corpora" "$OUT/questions.jsonl" "$OUT/qf-crawl4ai.json"
for h in all tune hold; do
    echo "== queryFilter ($h)"; HALF=$h python3 -I bench/rag/qf_eval.py "$DATA/corpora" "$OUT/questions.jsonl" "$OUT/qf-pagesift.json" "$OUT/qf-crawl4ai.json"
done | tee "$OUT/query_filter.txt"
