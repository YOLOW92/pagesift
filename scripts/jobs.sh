#!/usr/bin/env bash
# 按当前可用内存算出适合的并发数，用于 CARGO_BUILD_JOBS 等。
#   CARGO_BUILD_JOBS=$(scripts/jobs.sh) cargo build
#   scripts/jobs.sh <每个任务预计占用 MB，默认 1500> <给系统保留的 MB，默认 2048>
per_job=${1:-1500}
reserve=${2:-2048}
avail=$(awk '/^MemAvailable:/ {print int($2/1024)}' /proc/meminfo)
cores=$(nproc)
jobs=$(( (avail - reserve) / per_job ))
(( jobs < 1 )) && jobs=1
(( jobs > cores )) && jobs=$cores
echo "$jobs"
