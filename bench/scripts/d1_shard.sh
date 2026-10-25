#!/bin/bash
# 在容器内把 manifest 切成 N 份并用 N 个进程并行跑同一适配器，最后合成一份。
# 这样 Python 侧是真正的多进程（绕开 GIL），整体又受同一个 cgroup 内存上限约束。
# 用法: d1_shard.sh <nprocs> <adapter_cmd_prefix> <manifest> <out>
set -euo pipefail
N=$1; ADAPTER=$2; MAN=$3; OUT=$4
TMP=$(mktemp -d)
split -n l/"$N" -d -a 3 "$MAN" "$TMP/s."
pids=()
for f in "$TMP"/s.*; do
  $ADAPTER "$f" "$f.out" &
  pids+=($!)
done
rc=0
for p in "${pids[@]}"; do wait "$p" || rc=$?; done
cat "$TMP"/s.*.out > "$OUT"
rm -rf "$TMP"
echo "shards=$N rc=$rc out=$(wc -l < "$OUT")" >&2
