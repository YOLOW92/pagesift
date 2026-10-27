#!/usr/bin/env bash
# 浏览器内核对比：pagesift 用不同内核（engine=browser）跑 wide 和 huge 场景，以 Chrome 为基准。
# 每个内核每个场景最多 MEASURE_BUDGET_S 秒（默认 180），内存按 PSS 由 measure.py 看门狗保护。
# 用法：benchmarks/crawl/engines.sh [场景...]（默认 wide huge）
set -uo pipefail
cd "$(dirname "$0")"
source ./origin_ctl.sh   # 只结束本脚本起的源站
T=${T:-../data/tools}                       # 各浏览器内核所在目录
PAGES_DIR=${PAGES_DIR:-../data/crawl-pages}  # 源站用的页面
PORT=18080
export ENGINE=browser
export MEASURE_BUDGET_S=${MEASURE_BUDGET_S:-180}
export MEASURE_MAX_MB=${MEASURE_MAX_MB:-12288}
export MEASURE_ORIGIN=http://s0.localhost:$PORT/__served
export MEASURE_OUT=${MEASURE_OUT:-results-engines-$(date +%Y%m%d-%H%M%S).jsonl}
ISO='--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE *.localhost'

# 每个内核单独起一个会话（setsid），并给它一个私有的 TMPDIR。pagesift 起的浏览器在 TMPDIR 里建配置目录
# （pagesift-cdp-*），里面的 pagesift-owner 记着属主进程号、浏览器进程号和命令行（crates/pagesift-cdp/src/sweep.rs）。
# 结束时只停本次运行的进程：浏览器要命令行和记录一致、且在本次的会话里；再停本次会话里剩下的进程组。
# 都是先 SIGTERM，5 秒后还在再 SIGKILL。不按进程名杀进程（机器是共用的，用户自己的 Chrome 也在跑）。
RUN_TMP= RUN_SID=
stat_field() { sed 's/^.*) //' "/proc/$1/stat" 2>/dev/null | awk -v n="$2" '{print $n}'; }  # 2=ppid 3=pgrp 4=session
stop_group() { # 进程组号
    local g=$1
    kill -TERM -- "-$g" 2>/dev/null || return 0
    for _ in $(seq 1 50); do kill -0 -- "-$g" 2>/dev/null || return 0; sleep 0.1; done
    kill -KILL -- "-$g" 2>/dev/null || true
}
stop_run() {
    [ -n "$RUN_SID" ] || return 0
    local f pid g
    for f in "$RUN_TMP"/pagesift-cdp-*/pagesift-owner; do
        [ -f "$f" ] || continue
        pid=$(sed -n 2p "$f")
        [ -n "$pid" ] && [ "$(stat_field "$pid" 4)" = "$RUN_SID" ] || continue
        tail -n +3 "$f" | cmp -s - "/proc/$pid/cmdline" || continue
        stop_group "$(stat_field "$pid" 3)"
    done
    stop_group "$RUN_SID"
    for pid in $(ls /proc | grep -E '^[0-9]+$'); do   # 本次会话里还剩的（例如没来得及写 pagesift-owner 的浏览器）
        [ "$(stat_field "$pid" 4)" = "$RUN_SID" ] && stop_group "$(stat_field "$pid" 3)"
    done
    rm -rf "$RUN_TMP"
    RUN_TMP= RUN_SID=
}
trap 'stop_run; stop_origin' EXIT

# 等源站真正能应答再开始（页面多时启动要几秒；固定 sleep 不够，第一个工具会连不上）
wait_origin() { # 端口
    for _ in $(seq 1 240); do
        curl -s -o /dev/null -H "Host: s0.localhost" "http://[::1]:$1/__served" && return 0
        sleep 0.5
    done
    echo "origin on port $1 did not start" >&2
    return 1
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
        # 生产路径：PATH 里自动探测，Lightpanda → moli → chrome-headless-shell 回退
        auto) env=(BROWSER_LOOPBACK=1 "PATH=$(realpath $T/lightpanda):$(realpath $T/moli/moli-v1.1.18-x86_64-unknown-linux-gnu):$(realpath $T/chs/chrome-headless-shell-linux64):$PATH") ;;
        obscura) env=("BROWSER_EXE=$T/obscura/obscura"
            "BROWSER_ARGS=--allow-private-network serve --host 127.0.0.1 --port {port} --proxy http://[::1]:$PORT"
            BROWSER_MAX=4 BROWSER_TABS=4) ;;
    esac
    RUN_TMP=$(mktemp -d)
    # 后台启动时本进程不是进程组组长，setsid 不再 fork：$! 就是新会话号（也是进程组号）
    env TMPDIR="$RUN_TMP" "${env[@]}" setsid python3 -I measure.py "$scen/$name" -- ${PAGESIFT_BIN:-../../target/release/examples/crawl_bench} $PORT "$hosts" "$pages" 0 "$ph" 2>/dev/null &
    RUN_SID=$!
    wait "$RUN_SID"
    stop_run
}

for s in ${@:-wide huge}; do
    case $s in
        wide) hosts=200 pages=100 ph=8 ;;
        huge) hosts=1000 pages=50 ph=4 ;;
        smoke) hosts=3 pages=10 ph=2 ;;   # 冒烟
    esac
    start_origin target/release/origin $PORT "$PAGES_DIR" $PORT $hosts $pages 10
    wait_origin $PORT
    # 默认：各内核单独跑，加上生产路径的自动回退链（auto）
    for e in ${ENGINES:-chrome chs lightpanda moli auto obscura}; do engine "$e" "$s" "$hosts" "$pages" "$ph"; done
    stop_origin
done
echo "results in benchmarks/crawl/$MEASURE_OUT"
