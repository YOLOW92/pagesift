#!/usr/bin/env bash
# 分批跑 JS 抽取工具：每 N 页（默认 50）开一个新的 node 进程，最后把输出接起来。
# JSDOM 处理大量大页面时内存一路涨，分批后每批结束内存就释放；若单页触发 V8 OOM 或超时，记录该页为空并继续剩余页面。
#   benchmarks/web-extraction/compare/js_sharded.sh <工具> <node_modules 所在目录> <manifest.jsonl> <out.jsonl> [每批页数]
set -uo pipefail
tool=$1 node_dir=$2 manifest=$3 out=$4 n=${5:-50}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
split -l "$n" -d -a 4 "$manifest" "$tmp/part."
: > "$out.partial"
for part in "$tmp"/part.*; do
    [ -f "$part" ] || continue
    cur="$part"
    while [ -s "$cur" ]; do
        : > "$cur.out"
        timeout 90s node --max-old-space-size=${NODE_MAX_OLD_SPACE_SIZE:-768} "$(dirname "$0")/js_adapter.mjs" "$tool" "$node_dir" "$cur" "$cur.out" 2>/dev/null
        rc=$?
        [ -f "$cur.out" ] && cat "$cur.out" >> "$out.partial"
        if [ "$rc" -eq 0 ]; then
            break
        fi
        done_lines=$(wc -l < "$cur.out" 2>/dev/null || echo 0)
        total_lines=$(wc -l < "$cur")
        bad_line=$((done_lines + 1))
        if [ "$bad_line" -le "$total_lines" ]; then
            bad_id=$(sed -n "${bad_line}p" "$cur" | python3 -c "import sys,json; print(json.loads(sys.stdin.read())['id'])" 2>/dev/null || true)
            [ -n "$bad_id" ] && printf '{"id":"%s","markdown":"","ms":null}\n' "$bad_id" >> "$out.partial"
        fi
        next_line=$((done_lines + 2))
        if [ "$next_line" -le "$total_lines" ]; then
            tail -n +"$next_line" "$cur" > "$cur.next"
            mv "$cur.next" "$cur"
        else
            break
        fi
    done
done
mv "$out.partial" "$out"
