#!/usr/bin/env bash
# 按 AiSeek 主 VPS 的限额起 pagesift 的 /v1/fetch 适配器（和 ~/crawl-bench/server.sh 同口径）：
# 6 核（taskset）、10 GB 内存（cgroup MemoryMax）、CPU 权重 25。
# 用法：server.sh <pages> <port> [allow_hosts]
#   pages：浏览器进程上限；0 表示让 pagesift 自适应。
# 浏览器内核从 ENGINES_PATH（冒号分隔的目录）里自动探测，默认用 benchmarks/data/tools 下的 Lightpanda、moli、chrome-headless-shell。
set -euo pipefail
PAGES=${1:-4}
PORT=${2:-8100}
ALLOW=${3:-}
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
T=$ROOT/benchmarks/data/tools
ENGINES_PATH=${ENGINES_PATH:-$T/lightpanda:$T/moli/moli-v1.1.18-x86_64-unknown-linux-gnu:$T/chs/chrome-headless-shell-linux64}
[ "$PAGES" = 0 ] && PAGES=
exec systemd-run --user --scope -q -p MemoryMax=10G -p CPUWeight=25 \
  taskset -c 0-5 env \
  PATH="$ENGINES_PATH:$PATH" \
  PORT=$PORT PAGES=$PAGES ALLOW_HOSTS=$ALLOW \
  TOKEN=bench-token-local-0123456789abcdef \
  "$ROOT/target/aiseek-bench/release/aiseek_fetch"
