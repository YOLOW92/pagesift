#!/usr/bin/env bash
# 把清单切成小份放进队列，按内存动态决定同时跑几个进程：
# 每启动一个新进程前，确认可用内存大于"保留量 + 当前进程的最大实际占用"。
# 报告总墙钟时间、各进程峰值内存之和和最多同时运行的进程数。
#   bench/compare/parallel.sh <最多进程数> <manifest> <out.jsonl> -- <adapter 命令；最后补上 shard 清单和 shard 输出>
# 环境变量：PAR_RESERVE_MB（默认 3072），PAR_GUESS_MB（第一个进程跑起来前的占用估计，默认 500）
set -euo pipefail
max=$1; manifest=$2; out=$3; shift 4
reserve=${PAR_RESERVE_MB:-3072}
guess=${PAR_GUESS_MB:-500}
tmp=$(mktemp -d)
lines=$(wc -l < "$manifest")
shards=$(( max * 4 ))
(( shards > lines )) && shards=$lines
split -n l/"$shards" -d -a 3 "$manifest" "$tmp/shard."
queue=("$tmp"/shard.[0-9][0-9][0-9])

avail_mb() { awk '/^MemAvailable:/ {print int($2/1024)}' /proc/meminfo; }
rss_mb() { awk '/^VmRSS:/ {print int($2/1024)}' "/proc/$1/status" 2>/dev/null || echo 0; }

declare -A pids=()
peak_concurrency=0
biggest=$guess
start=$(date +%s.%N)
while (( ${#queue[@]} > 0 || ${#pids[@]} > 0 )); do
    for p in "${!pids[@]}"; do
        if kill -0 "$p" 2>/dev/null; then
            r=$(rss_mb "$p"); (( r > biggest )) && biggest=$r
        else
            wait "$p" || true; unset "pids[$p]"
        fi
    done
    if (( ${#queue[@]} > 0 && ${#pids[@]} < max )) && (( $(avail_mb) > reserve + biggest )); then
        s=${queue[0]}; queue=("${queue[@]:1}")
        /usr/bin/time -f "%M" -o "$s.rss" "$@" "$s" "$s.out" 2>/dev/null &
        pids[$!]=1
        (( ${#pids[@]} > peak_concurrency )) && peak_concurrency=${#pids[@]}
        continue
    fi
    sleep 0.2
done
end=$(date +%s.%N)
for s in "$tmp"/shard.[0-9][0-9][0-9]; do
    # Crawl4AI 的适配脚本输出一个目录，取其中的 fit_markdown
    if [ -d "$s.out" ]; then cat "$s.out"/crawl4ai-fit.jsonl; else cat "$s.out"; fi
done > "$out"
rss=$(cat "$tmp"/shard.*.rss | awk '{s+=$1} END {print s}')
echo "max_processes=$max peak_concurrency=$peak_concurrency wall_s=$(echo "$end - $start" | bc) sum_peak_rss_kb=$rss biggest_rss_mb=$biggest pages=$(wc -l < "$out")"
rm -rf "$tmp"
