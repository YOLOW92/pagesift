#!/usr/bin/env bash
# 整站爬取对决：对每个场景启动源站，依次（不并行）运行每个工具，结果写进 results-<时间>.jsonl。
# 用法：bench/crawl/run_all.sh [场景...]
#   wide  200 站 × 100 页，不限速，每主机 8 并发，10 ms 延迟（默认）
#   huge  1000 站 × 50 页，不限速，每主机 4 并发，10 ms 延迟（默认）
#   polite 100 站 × 50 页，每主机每秒 1 个请求：只在验收"同样守规矩"门槛时单独跑
# 每个工具最多跑 MEASURE_BUDGET_S 秒（默认 600），内存超过可用内存减 2 GB 就整树杀掉（见 measure.py）。
# 计时要在机器空闲时跑：工具一个一个来，每个都能用满所有核。
set -euo pipefail
cd "$(dirname "$0")"
ROOT=../..
PAGES_DIR=${PAGES_DIR:-$ROOT/bench/data/crawl-pages}
PORT=18080
export MEASURE_OUT=${MEASURE_OUT:-results-$(date +%Y%m%d-%H%M%S).jsonl}
export CRAWL4AI_NO_CLOUD_NOTICE=1
# Crawl4AI 默认给 Chromium 加 --no-sandbox；评测里一律开着沙箱（和 pagesift 一样）
export CRAWL4AI_CHROMIUM_SANDBOX=true
export MEASURE_BUDGET_S=${MEASURE_BUDGET_S:-600}
export MEASURE_ORIGIN=http://s0.localhost:$PORT/__served
C4A=${C4A:-$ROOT/bench/data/venvs/crawl4ai/bin/python}
CRW=${CRW:-$ROOT/bench/data/tools/bin/crw}
# Scrapy 用的 Python（装了 scrapy 的虚拟环境）
SCRAPY_PY=${SCRAPY_PY:-$ROOT/bench/data/fresh/tools/py/bin/python}

# 等源站真正能应答再开始（页面多时启动要几秒；固定 sleep 不够，第一个工具会连不上）
wait_origin() { # 端口
    for _ in $(seq 1 240); do
        curl -s -o /dev/null -H "Host: s0.localhost" "http://[::1]:$1/__served" && return 0
        sleep 0.5
    done
    echo "origin on port $1 did not start" >&2
    return 1
}

run() { # 场景名 hosts pages interval per_host latency 工具...
    local name=$1 hosts=$2 pages=$3 interval=$4 per_host=$5 latency=$6; shift 6
    pkill -x origin || true; sleep 0.5
    target/release/origin "$PAGES_DIR" $PORT "$hosts" "$pages" "$latency" 2>/dev/null &
    wait_origin $PORT
    for tool in "$@"; do
        local label="$name/$tool"
        case $tool in
            pagesift) python3 -I measure.py "$label" -- $ROOT/target/release/examples/crawl_bench $PORT "$hosts" "$pages" "$interval" "$per_host" ;;
            crw) python3 -I measure.py "$label" -- python3 -I crw_crawl.py "$CRW" $PORT "$hosts" "$pages" "$interval" "$per_host" ;;
            # Spider 并发上限为 1 时每站只访问 5 个链接就停（它自己的行为）；设了间隔后它本来就逐个请求，
            # 所以给 2，实际仍是每主机一个请求接一个
            spider) python3 -I measure.py "$label" -- target/release/spider_crawl $PORT "$hosts" "$pages" "$interval" "$(( per_host < 2 ? 2 : per_host ))" ;;
            crawl4ai-http) python3 -I measure.py "$label" -- "$C4A" crawl4ai_crawl.py $PORT "$hosts" "$pages" "$interval" http "$per_host" ;;
            crawl4ai-multi) python3 -I measure.py "$label" -- python3 -I crawl4ai_multi.py "$C4A" $PORT "$hosts" "$pages" "$interval" http "$per_host" ;;
            scrapy) python3 -I measure.py "$label" -- "$SCRAPY_PY" -I scrapy_crawl.py $PORT "$hosts" "$pages" "$interval" "$per_host" ;;
            crawl4ai-browser) python3 -I measure.py "$label" -- "$C4A" crawl4ai_crawl.py $PORT "$hosts" "$pages" "$interval" browser "$per_host" ;;
        esac || true   # 一个工具失败不影响后面的工具
    done
    pkill -x origin || true
}

for s in "${@:-wide huge}"; do
    for one in $s; do
        case $one in
            wide)   run wide 200 100 0 8 10 ${TOOLS:-pagesift crw spider scrapy crawl4ai-http crawl4ai-multi crawl4ai-browser} ;;
            huge)   run huge 1000 50 0 4 10 ${TOOLS:-pagesift crw spider scrapy crawl4ai-http crawl4ai-multi} ;;
            polite) run polite 100 50 1000 1 50 pagesift crw spider crawl4ai-http crawl4ai-browser ;;
            # 冒烟：3 站 × 10 页，每个工具都跑一遍，正式跑之前查脚本和工具能不能跑通
            smoke)  run smoke 3 10 0 2 5 ${TOOLS:-pagesift crw spider scrapy crawl4ai-http crawl4ai-multi crawl4ai-browser} ;;
        esac
    done
done
echo "results in bench/crawl/$MEASURE_OUT"
