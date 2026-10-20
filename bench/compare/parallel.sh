#!/usr/bin/env bash
# 把清单切成 N 份，每份一个进程同时跑同一个适配命令，报告总墙钟时间和各进程峰值内存之和。
#   bench/compare/parallel.sh <N> <manifest> <out.jsonl> -- <adapter 命令，最后两个参数会补上 shard 清单和 shard 输出>
set -euo pipefail
n=$1; manifest=$2; out=$3; shift 4
tmp=$(mktemp -d)
split -n l/"$n" -d "$manifest" "$tmp/shard."
start=$(date +%s.%N)
for s in "$tmp"/shard.[0-9]*; do
    case "$s" in *.out|*.rss) continue ;; esac
    /usr/bin/time -f "%M" -o "$s.rss" "$@" "$s" "$s.out" 2>/dev/null &
done
wait
end=$(date +%s.%N)
for s in "$tmp"/shard.*; do
    case "$s" in *.out|*.rss) continue ;; esac
    # Crawl4AI 的适配脚本输出一个目录，取其中的 fit_markdown
    if [ -d "$s.out" ]; then cat "$s.out"/crawl4ai-fit.jsonl; else cat "$s.out"; fi
done > "$out"
rss=$(cat "$tmp"/shard.*.rss | awk '{s+=$1} END {print s}')
echo "processes=$n wall_s=$(echo "$end - $start" | bc) total_peak_rss_kb=$rss pages=$(wc -l < "$out")"
rm -rf "$tmp"
