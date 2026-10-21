#!/usr/bin/env bash
# 按物理核绑核并行：从标准输入每行读一条命令，同时运行的命令数等于物理核数，每条命令独占一个
# 物理核（只用每个物理核的第一个逻辑 CPU，超线程兄弟保持空闲），一条结束再把它的核分给下一条。
# 单线程计时在这种条件下对所有工具相同；需要全核的吞吐测试不要用它。
#   printf '%s\n' "cmd1 ..." "cmd2 ..." | bench/fresh/pinned.sh
#   bench/fresh/pinned.sh --cores     # 只打印用到的逻辑 CPU
set -uo pipefail
mapfile -t CORES < <(lscpu -p=CPU,CORE | grep -v '^#' | sort -t, -k2,2n -k1,1n | awk -F, '!seen[$2]++ {print $1}')
if [ "${1:-}" = --cores ]; then
    echo "physical cores: ${#CORES[@]}, CPUs used: ${CORES[*]}"
    exit 0
fi
declare -A busy=()   # 逻辑 CPU → pid
status=0
reap() {
    local c
    for c in "${!busy[@]}"; do
        if ! kill -0 "${busy[$c]}" 2>/dev/null; then
            wait "${busy[$c]}" || status=1
            unset "busy[$c]"
        fi
    done
}
while IFS= read -r cmd; do
    [ -z "$cmd" ] && continue
    while :; do
        reap
        free=""
        for c in "${CORES[@]}"; do
            [ -z "${busy[$c]:-}" ] && { free=$c; break; }
        done
        [ -n "$free" ] && break
        sleep 0.2
    done
    taskset -c "$free" bash -c "$cmd" &
    busy[$free]=$!
done
while [ ${#busy[@]} -gt 0 ]; do reap; sleep 0.2; done
exit $status
