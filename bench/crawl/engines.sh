#!/usr/bin/env bash
# 浏览器内核对比：pagesift 用不同内核（engine=browser）跑 wide 和 huge 场景，以 Chrome 为基准。
# 每个内核每个场景最多 MEASURE_BUDGET_S 秒（默认 180），内存按 PSS 由 measure.py 看门狗保护。
# 用法：bench/crawl/engines.sh [场景...]（默认 wide huge）
set -uo pipefail
cd "$(dirname "$0")"
T=../data/tools
PORT=18080
export ENGINE=browser
export MEASURE_BUDGET_S=${MEASURE_BUDGET_S:-180}
export MEASURE_ORIGIN=http://s0.localhost:$PORT/__served
export MEASURE_OUT=${MEASURE_OUT:-results-engines-$(date +%Y%m%d-%H%M%S).jsonl}
ISO='--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE *.localhost'

# 按可执行文件路径结束浏览器进程（不用 pkill -f：模式会匹配到调用它的 shell 自己）
cleanup() {
    for pid in $(ls /proc | grep -E '^[0-9]+$'); do
        exe=$(readlink "/proc/$pid/exe" 2>/dev/null) || continue
        case $exe in
            /opt/google/chrome/*|*/chrome-headless-shell-linux64/*|*/lightpanda-x86_64-linux|*/obscura|*/obscura-worker|*/moli)
                kill -9 "$pid" 2>/dev/null ;;
        esac
    done
    find /tmp -maxdepth 1 \( -name ".com.google.Chrome.*" -o -name "com.google.Chrome.*" -o -name "pagesift-cdp-*" \) \
        -user "$(id -u)" -exec rm -rf {} + 2>/dev/null || true
}

engine() { # 名字 场景 hosts pages per_host
    local name=$1 scen=$2 hosts=$3 pages=$4 ph=$5
    local -a env=()
    case $name in
        chrome) env=("BROWSER_EXTRA=$ISO") ;;
        chs) env=("BROWSER_EXE=$T/chs/chrome-headless-shell-linux64/chrome-headless-shell" "BROWSER_EXTRA=$ISO") ;;
        lightpanda) env=("BROWSER_EXE=$T/lightpanda/lightpanda-x86_64-linux"
            "BROWSER_ARGS=serve --host 127.0.0.1 --port {port} --block-cidrs 0.0.0.0/0 --block-cidrs ::/0 --block-cidrs -::1/128 --block-cidrs -127.0.0.0/8"
            BROWSER_MAX=16 BROWSER_TABS=1 BROWSER_RECYCLE=1) ;;
        # 内核自己挡网络（不走 CDP 拦截）：只放行本机（源站在 ::1）
        lightpanda-eg) env=("BROWSER_EXE=$T/lightpanda/lightpanda-x86_64-linux"
            "BROWSER_ARGS=serve --host 127.0.0.1 --port {port} --block-cidrs 0.0.0.0/0 --block-cidrs ::/0 --block-cidrs -::1/128 --block-cidrs -127.0.0.0/8"
            BROWSER_MAX=16 BROWSER_TABS=1 BROWSER_RECYCLE=1 BROWSER_ENGINE_GUARD=1) ;;
        moli) env=("BROWSER_EXE=$T/moli/moli-v1.1.18-x86_64-unknown-linux-gnu/moli"
            "BROWSER_ARGS=serve --host 127.0.0.1 --port {port} --block-cidrs=0.0.0.0/2,64.0.0.0/3,96.0.0.0/4,112.0.0.0/5,120.0.0.0/6,124.0.0.0/7,126.0.0.0/8,128.0.0.0/1,2000::/3,fc00::/7,fe80::/10"
            BROWSER_MAX=16 BROWSER_TABS=1 BROWSER_ENGINE_GUARD=1) ;;
        obscura) env=("BROWSER_EXE=$T/obscura/obscura"
            "BROWSER_ARGS=--allow-private-network serve --host 127.0.0.1 --port {port} --proxy http://[::1]:$PORT"
            BROWSER_MAX=4 BROWSER_TABS=4) ;;
    esac
    env "${env[@]}" python3 -I measure.py "$scen/$name" -- ../../target/release/examples/crawl_bench $PORT "$hosts" "$pages" 0 "$ph" 2>/dev/null
    cleanup
}

for s in ${@:-wide huge}; do
    case $s in
        wide) hosts=200 pages=100 ph=8 ;;
        huge) hosts=1000 pages=50 ph=4 ;;
    esac
    pkill -x origin || true
    sleep 0.5
    target/release/origin ../data/crawl-pages $PORT $hosts $pages 10 2>/dev/null &
    sleep 2
    for e in ${ENGINES:-chrome chs lightpanda obscura}; do engine "$e" "$s" "$hosts" "$pages" "$ph"; done
    pkill -x origin || true
done
echo "results in bench/crawl/$MEASURE_OUT"
