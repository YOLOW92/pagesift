#!/usr/bin/env bash
# 分批跑 JS 抽取工具：每 N 页（默认 100）开一个新的 node 进程，最后把输出接起来。
# JSDOM 处理大量大页面时内存一路涨，一个进程跑几千页会撑爆 Node 的堆、整批结果丢失；分批后每批结束内存就释放，
# 单页耗时不受影响（每页仍是一次完整的解析和抽取）。
#   bench/compare/js_sharded.sh <工具> <node_modules 所在目录> <manifest.jsonl> <out.jsonl> [每批页数]
set -euo pipefail
tool=$1 node_dir=$2 manifest=$3 out=$4 n=${5:-100}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
split -l "$n" -d -a 4 "$manifest" "$tmp/part."
: > "$out.partial"
for part in "$tmp"/part.*; do
    # 堆上限 1.5 GB：一批 100 页够用，万一超了只丢这一批，不会拖垮整台机器
    node --max-old-space-size=1536 "$(dirname "$0")/js_adapter.mjs" "$tool" "$node_dir" "$part" "$part.out"
    cat "$part.out" >> "$out.partial"
done
mv "$out.partial" "$out"
