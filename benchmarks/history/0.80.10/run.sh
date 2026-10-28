#!/usr/bin/env bash
# 用法: run.sh <二进制> <输出目录> <limit|auto> [主题...]
# 和 0.80.9/run.sh 相同（69 校 × 5 个方向，crawl --query --adaptive --ignore-robots，8 个并行），
# 只是上限可变：auto 表示不给 --limit（0.80.10 起由自适应停止决定页数，只留 500 页的保险）。
# 结果在 <输出目录>/<主题>/<站名>/index.jsonl，日志 <站名>.err（最后一行是停止原因）。
set -u
BIN=$1; OUT=$2; LIMIT=$3; shift 3
TOPICS=${*:-intl grad housing cs library}
SITES=${SITES:-$(dirname "$0")/sites.txt}
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
  if [ "$LIMIT" = auto ]; then L=""; else L="--limit $LIMIT"; fi
  timeout 1200 "$BIN" crawl "$url" --query "$QUERY" --adaptive $L --ignore-robots --out "$dir/$name" >"$dir/$name.out" 2>"$dir/$name.err"
  echo "$topic $name rc=$?"
}
export -f run_one; export BIN OUT LIMIT
for t in $TOPICS; do
  export QUERY="${Q[$t]}"
  grep -v '^#' "$SITES" | awk -v t="$t" '{print t, $1, $2}' | xargs -P 8 -L 1 bash -c 'run_one "$0" "$1" "$2"'
done
