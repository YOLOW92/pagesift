#!/usr/bin/env bash
# 分块和检索评测（数据：Chroma chunking_evaluation，5 份语料、472 个问题，下载到 benchmarks/data/fresh/data/chunkeval）。
#   1. 分块：每种分块器按约 200 词切块，同一个 bm25s 在全部块上检索，按词数预算比召回、精确率、IoU（eval.py）；
#   2. queryFilter：每个问题在它所属的文档里筛选，比"原样输出"的召回和输出词数，以及同预算下的排序质量（qf_eval.py）。
# 结果写到 $OUT（默认 benchmarks/rag/out）；HALF=tune/hold/all 选问题子集（默认三个都跑）。
set -euo pipefail
cd "$(dirname "$0")/../.."
OUT=${OUT:-bench/rag/out}
DATA=${DATA:-bench/data/fresh/data/chunkeval}
PY=${PY:-bench/data/fresh/tools/py/bin/python}     # bm25s、langchain-text-splitters、chonkie、semchunk
C4A=${C4A:-bench/data/fresh/tools/c4ai/bin/python}   # Crawl4AI
SIZE=${SIZE:-200}
rm -rf "$OUT"; mkdir -p "$OUT/chunks"
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
cargo build --release -q -p pagesift-extract --example chunk_spans --example qf_ranks
CORPORA=$(ls "$DATA"/corpora/*.md | tr "\n" " ")
{
    for spec in "recursive:{\"type\":\"recursive\",\"size\":$SIZE}" "fixed:{\"type\":\"fixed_tokens\",\"size\":$SIZE}" \
                "sliding:{\"type\":\"sliding_window\",\"window\":$SIZE,\"step\":$((SIZE / 2))}" \
                "paragraphs:{\"type\":\"paragraphs\"}" "sentences:{\"type\":\"sentences\"}"; do
        printf "target/release/examples/chunk_spans '%s' $OUT/chunks/pagesift-%s.json $CORPORA > $OUT/t-pagesift-%s.txt 2>&1\n" \
            "${spec#*:}" "${spec%%:*}" "${spec%%:*}"
    done
    for m in langchain-recursive chonkie-recursive chonkie-sentence chonkie-token semchunk llamaindex-sentence; do
        echo "$PY -I benchmarks/rag/chunkers.py $OUT/chunks $SIZE $m -- $CORPORA > $OUT/t-$m.txt 2>&1"
    done
    for m in crawl4ai-fixed crawl4ai-sliding crawl4ai-regex; do
        echo "$C4A -I benchmarks/rag/chunkers.py $OUT/chunks $SIZE $m -- $CORPORA > $OUT/t-$m.txt 2>&1"
    done
} | benchmarks/common/fresh/pinned.sh   # 每个分块器各占一个物理核
{ for f in "$OUT"/t-pagesift-*.txt; do sed "s/^/$(basename "$f" .txt | sed 's/^t-//'): /" "$f"; done
  cat "$OUT"/t-[!p]*.txt; } > "$OUT/timing.txt"
for h in all tune hold; do
    echo "== chunking ($h)"; HALF=$h "$PY" -I benchmarks/rag/eval.py "$DATA" "$OUT/chunks"
done | tee "$OUT/chunking.txt"
python3 -I benchmarks/rag/qf_prepare.py "$DATA" "$OUT/questions.jsonl"
target/release/examples/qf_ranks "$DATA/corpora" "$OUT/questions.jsonl" "$OUT/qf-pagesift.json" 2>> "$OUT/qf-timing.txt"
PAGESIFT_WORKERS=32 "$C4A" -I benchmarks/rag/qf_crawl4ai.py "$DATA/corpora" "$OUT/questions.jsonl" "$OUT/qf-crawl4ai.json" 2>> "$OUT/qf-timing.txt"
for h in all tune hold; do
    echo "== queryFilter ($h)"; HALF=$h python3 -I benchmarks/rag/qf_eval.py "$DATA/corpora" "$OUT/questions.jsonl" "$OUT/qf-pagesift.json" "$OUT/qf-crawl4ai.json"
done | tee "$OUT/query_filter.txt"
