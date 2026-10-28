#!/usr/bin/env bash
# 第 2 版 D（定向爬取，RULES.md 第二节）：samples/d.tsv 的 30 所学校 × 0.80.9 的 5 个方向，每个工具每次 12 页预算。
#   MIX_PAGESIFT_BIN=<0.80.9 二进制> benchmarks/mix/v2/run_d.sh <输出根目录> [并行数，默认 6]
# 结果：<输出根目录>/<工具>/<方向>/<学校 id>/index.jsonl（grade.py 的格式），每次的耗时在同目录 summary.json / .time。
# pagesift 遵守 robots.txt（默认）；Crawl4AI 的 AdaptiveCrawler 打不开 robots 检查（见 d_crawl4ai.py）。单次上限 600 s。
set -u
here=$(cd "$(dirname "$0")" && pwd)
OUT=$1; P=${2:-6}
mix=${MIX_TOOLS:-$HOME/mix-tools}
: "${MIX_PAGESIFT_BIN:?要给 MIX_PAGESIFT_BIN}"
declare -A Q=(
  [intl]="international students admission tuition scholarship visa"
  [grad]="graduate admission requirements PhD application deadline"
  [housing]="student housing accommodation residence"
  [cs]="computer science department faculty research"
  [library]="library opening hours borrowing"
)
run_one() {  # tool topic id url
  local tool=$1 topic=$2 id=$3 url=$4 q=${Q[$2]} d=$OUT/$1/$2/$3 t0
  mkdir -p "$(dirname "$d")"; t0=$(date +%s.%N)
  if [[ $tool == pagesift ]]; then
    PAGESIFT_CONFIG= timeout 600 "$MIX_PAGESIFT_BIN" crawl "$url" --query "$q" --adaptive --limit 12 --out "$d" > "$d.out" 2> "$d.err"
  else
    PLAYWRIGHT_BROWSERS_PATH=$mix/ms-playwright CRAWL4AI_NO_CLOUD_NOTICE=1 timeout 600 "$mix/py/crawl4ai/bin/python" -I "$here/d_crawl4ai.py" "$url" "$q" "$d" > "$d.out" 2> "$d.err"
  fi
  local rc=$?
  awk -v a="$t0" -v b="$(date +%s.%N)" -v rc=$rc 'BEGIN{printf "%.1f rc=%s\n", b-a, rc}' > "$d.time"
  echo "$tool $topic $id rc=$rc"
}
export -f run_one; export OUT MIX_PAGESIFT_BIN here mix
export Q_intl="${Q[intl]}" Q_grad="${Q[grad]}" Q_housing="${Q[housing]}" Q_cs="${Q[cs]}" Q_library="${Q[library]}"
# 作业表：两个工具交错排，同一学校同一方向的两个工具相邻（时间上尽量接近）
tail -n +2 "$here/samples/d.tsv" | while IFS=$'\t' read -r id name home rest; do
  for topic in intl grad housing cs library; do
    for tool in pagesift crawl4ai; do echo "$tool $topic $id $home"; done
  done
done | xargs -P "$P" -L 1 bash -c 'declare -A Q=([intl]="$Q_intl" [grad]="$Q_grad" [housing]="$Q_housing" [cs]="$Q_cs" [library]="$Q_library"); run_one "$0" "$1" "$2" "$3"'
echo "D 全部完成"
