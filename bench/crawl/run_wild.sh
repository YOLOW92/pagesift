#!/usr/bin/env bash
# 真实网络模拟对决：模拟源站（src/bin/wild.rs）按固定种子给每站随机加陷阱、重复 URL、二进制链接、
# robots 禁区、偶发 503 和限流。每个工具单独启动一次源站（统计从零开始），跑完取 /__report。
# 用法：bench/crawl/run_wild.sh [场景...]；SEED 换一批随机站点（默认 1）。
#   fast    40 站 × 60 页，上限 60 页/站，不限间隔，每主机 4 并发，基准延迟 20 ms
#   polite  40 站 × 60 页，上限 60 页/站，每主机间隔 200 ms，1 并发，基准延迟 20 ms
# 结果：wild-<时间>/<场景>-<工具>.json（源站报告）和 measure.py 的 jsonl；汇总用 wild_score.py。
set -euo pipefail
cd "$(dirname "$0")"
ROOT=../..
PAGES_DIR=${PAGES_DIR:-$ROOT/bench/data/crawl-pages}
PORT=18090
SEED=${SEED:-1}
OUT=${OUT:-wild-$(date +%Y%m%d-%H%M%S)}
mkdir -p "$OUT"
export MEASURE_OUT=$OUT/measure.jsonl
export CRAWL4AI_NO_CLOUD_NOTICE=1
export CRAWL4AI_CHROMIUM_SANDBOX=true   # 不让 Crawl4AI 关掉浏览器沙箱
export MEASURE_BUDGET_S=${MEASURE_BUDGET_S:-600}
export MEASURE_ORIGIN=http://s0.localhost:$PORT/__served
C4A=${C4A:-$ROOT/bench/data/venvs/crawl4ai/bin/python}
CRW=${CRW:-$ROOT/bench/data/tools/bin/crw}
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
    for tool in "$@"; do
        pkill -x wild || true; sleep 0.5
        target/release/wild "$PAGES_DIR" $PORT "$hosts" "$pages" "$latency" "$SEED" 2>/dev/null &
        wait_origin $PORT
        local label="wild-$name/$tool"
        case $tool in
            pagesift) python3 -I measure.py "$label" -- ${PAGESIFT_BIN:-$ROOT/target/release/examples/crawl_bench} $PORT "$hosts" "$pages" "$interval" "$per_host" ;;
            crw) python3 -I measure.py "$label" -- python3 -I crw_crawl.py "$CRW" $PORT "$hosts" "$pages" "$interval" "$per_host" ;;
            spider) python3 -I measure.py "$label" -- target/release/spider_crawl $PORT "$hosts" "$pages" "$interval" "$(( per_host < 2 ? 2 : per_host ))" ;;
            scrapy) python3 -I measure.py "$label" -- "$SCRAPY_PY" -I scrapy_crawl.py $PORT "$hosts" "$pages" "$interval" "$per_host" ;;
            crawl4ai-http) python3 -I measure.py "$label" -- "$C4A" crawl4ai_crawl.py $PORT "$hosts" "$pages" "$interval" http "$per_host" ;;
        esac || true
        curl -s -H "Host: s0.localhost" "http://[::1]:$PORT/__report" > "$OUT/$name-$tool.json"
        curl -s -H "Host: s0.localhost" "http://[::1]:$PORT/__log" > "$OUT/$name-$tool.log"
    done
    pkill -x wild || true
}

for s in ${@:-fast polite}; do
    case $s in
        fast)   run fast 40 60 0 4 20 ${TOOLS:-pagesift crw spider scrapy crawl4ai-http} ;;
        polite) run polite 40 60 200 1 20 ${TOOLS:-pagesift crw spider scrapy crawl4ai-http} ;;
        smoke)  run smoke 3 10 0 2 5 ${TOOLS:-pagesift crw spider scrapy crawl4ai-http} ;;   # 冒烟
    esac
done
python3 -I wild_score.py "$OUT"
