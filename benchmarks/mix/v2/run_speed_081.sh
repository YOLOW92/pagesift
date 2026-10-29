#!/usr/bin/env bash
# 只复测 pagesift 0.81 的速度轮（0.80.9 的旧结果不动）：
#   MIX_PAGESIFT_BIN=<0.81 二进制> benchmarks/mix/v2/run_speed_081.sh
# 结果写进 $MIX/runs-v2/speed/w<路数>/<子类>/pagesift-081*，和 10-28 的旧工具目录并列。
# 16 路沿用第 2 版的做法：先跑 1 路，按实测峰值 × 1.5（最少 100 MB）生成预扣表，再跑 16 路。
set -euo pipefail
unset MIX_PER_WORKER_MB
here=$(cd "$(dirname "$0")/.." && pwd)
mix=${MIX_TOOLS:-$HOME/mix-tools}
: "${MIX_PAGESIFT_BIN:?要给 0.81 的 pagesift 二进制}"
export MIX_PAGESIFT_BIN
out=${MIX_OUT_ROOT:-$mix/runs-v2/speed}

run() {  # run <路数> <子集选项> <作业>...
    local w=$1 subset=$2; shift 2
    local specs=()
    for j in "$@"; do specs+=("$j@$w"); done
    MIX_OUT_ROOT=$out/w$w MIX_SUBSET=$subset python3 -I "$here/run_batch.py" "${specs[@]}"
}

web="a2:pagesift-081 a2:pagesift-081-serve"
crawl="a2:pagesift-081-crawl a2:pagesift-081-serve-crawl"
pdf="b1:pagesift-081-file b1:pagesift-081-serve-file"
office="b4:pagesift-081-file b4:pagesift-081-serve-file"

# 1 路：同时得到后面 16 路要用的内存预扣
run 1 "" $web $crawl
run 1 model $pdf
run 1 "" $office
MIX_SPEED_ROOT=$out MIX_TOOLS=$mix python3 -I "$here/v2/peaks.py" a2 b1 b4 > "$mix/runs-v2/peaks-081.json"
export MIX_PER_WORKER_MB="$mix/runs-v2/peaks-081.json"

# 16 路
run 16 "" $web $crawl
run 16 model $pdf
run 16 "" $office
echo "0.81 测速轮全部完成：$out"
