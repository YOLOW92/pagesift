#!/usr/bin/env bash
# 用法: run.sh <二进制> <输出目录>  对 sites.txt 里每一站跑 crawl --query --adaptive --limit 12，4 个并行
set -u
BIN=$1; OUT=$2; mkdir -p "$OUT"
Q="international students admission tuition scholarship visa"
run_one() {
  name=$1; url=$2
  timeout 420 "$BIN" crawl "$url" --query "$Q" --adaptive --limit 12 --out "$OUT/$name" >"$OUT/$name.out" 2>"$OUT/$name.err"
  echo "$name rc=$? $(tail -1 "$OUT/$name.err")"
}
export -f run_one; export BIN OUT Q
grep -v '^#' "$(dirname "$0")/sites.txt" | xargs -P 4 -L 1 bash -c 'run_one "$0" "$1"'
