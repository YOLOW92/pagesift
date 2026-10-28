#!/usr/bin/env bash
# 用法: run.sh <二进制> <输出目录> [主题...]
# 对 sites.txt 里每一站、每个主题跑 crawl --query --adaptive --limit 12 --ignore-robots，8 个并行。
# 命令行没有 --ignore-robots 的旧版本（0.80.5 以前）用 api_run.py 经 HTTP API 跑同样的请求。
# 结果在 <输出目录>/<主题>/<站名>/index.jsonl，日志 <站名>.err。
set -u
BIN=$1; OUT=$2; shift 2
TOPICS=${*:-intl grad housing cs library}
declare -A Q=(
  [intl]="international students admission tuition scholarship visa"
  [grad]="graduate admission requirements PhD application deadline"
  [housing]="student housing accommodation residence"
  [cs]="computer science department faculty research"
  [library]="library opening hours borrowing"
)
run_one() {
  topic=$1; name=$2; url=$3
  dir="$OUT/$topic"; mkdir -p "$dir"
  timeout 420 "$BIN" crawl "$url" --query "$QUERY" --adaptive --limit 12 --ignore-robots --out "$dir/$name" >"$dir/$name.out" 2>"$dir/$name.err"
  echo "$topic $name rc=$?"
}
export -f run_one; export BIN OUT
for t in $TOPICS; do
  export QUERY="${Q[$t]}"
  grep -v '^#' "$(dirname "$0")/sites.txt" | awk -v t="$t" '{print t, $1, $2}' | xargs -P 8 -L 1 bash -c 'run_one "$0" "$1" "$2"'
done
