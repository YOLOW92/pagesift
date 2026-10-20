#!/usr/bin/env bash
# 整站爬取对决：对每个场景启动源站，依次（不并行）运行每个工具，结果写进 results-<时间>.jsonl。
# 用法：bench/crawl/run_all.sh [场景...]   场景：polite fast scale（默认全部）
# 计时要在机器空闲时跑：工具一个一个来，每个都能用满所有核。
set -euo pipefail
cd "$(dirname "$0")"
ROOT=../..
PAGES_DIR=$ROOT/bench/data/crawl-pages
PORT=18080
export MEASURE_OUT=${MEASURE_OUT:-results-$(date +%Y%m%d-%H%M%S).jsonl}
export CRAWL4AI_NO_CLOUD_NOTICE=1
C4A=$ROOT/bench/data/venvs/crawl4ai/bin/python
CRW=$ROOT/bench/data/tools/bin/crw

run() { # 场景名 hosts pages interval per_host latency 工具...
    local name=$1 hosts=$2 pages=$3 interval=$4 per_host=$5 latency=$6; shift 6
    pkill -x origin || true; sleep 0.5
    target/release/origin "$PAGES_DIR" $PORT "$hosts" "$pages" "$latency" 2>/dev/null &
    sleep 1
    for tool in "$@"; do
        local label="$name/$tool"
        case $tool in
            pagesift) python3 -I measure.py "$label" -- $ROOT/target/release/examples/crawl_bench $PORT "$hosts" "$pages" "$interval" "$per_host" ;;
            crw) python3 -I measure.py "$label" -- python3 -I crw_crawl.py "$CRW" $PORT "$hosts" "$pages" "$interval" "$per_host" ;;
            spider) python3 -I measure.py "$label" -- target/release/spider_crawl $PORT "$hosts" "$pages" "$interval" "$per_host" ;;
            crawl4ai-http) timeout 3600 python3 -I measure.py "$label" -- "$C4A" crawl4ai_crawl.py $PORT "$hosts" "$pages" "$interval" http "$per_host" ;;
            crawl4ai-browser) timeout 3600 python3 -I measure.py "$label" -- "$C4A" crawl4ai_crawl.py $PORT "$hosts" "$pages" "$interval" browser "$per_host" ;;
        esac
    done
    pkill -x origin || true
}

for s in "${@:-polite fast scale}"; do
    for one in $s; do
        case $one in
            polite) run polite 100 50 1000 1 50 pagesift crw spider crawl4ai-http crawl4ai-browser ;;
            fast)   run fast 20 100 0 4 20 pagesift crw spider crawl4ai-http ;;
            scale)  run scale 200 50 0 2 20 pagesift crw spider crawl4ai-http ;;
        esac
    done
done
echo "results in bench/crawl/$MEASURE_OUT"
