#!/usr/bin/env bash
# 评测期间的整机内存看门狗：MemAvailable 低于阈值（默认 1500 MB）时，杀掉占内存最多的评测 worker 所在的 cgroup
# （systemd 的 mix-*.scope，连子进程一起），driver 把这一份记成 crash/oom 并换新 worker。每次动手写一行日志。
#   benchmarks/mix/v2/mem_guard.sh [阈值 MB] [日志]
min=${1:-1500}; log=${2:-$HOME/mix-tools/mem_guard.log}
cg=/sys/fs/cgroup/user.slice/user-$(id -u).slice/user@$(id -u).service
while true; do
    avail=$(awk '/MemAvailable/ {print int($2/1024)}' /proc/meminfo)
    if (( avail < min )); then
        big=$(find "$cg" -name 'mix-*.scope' -type d 2>/dev/null | while read -r d; do echo "$(cat "$d/memory.current" 2>/dev/null || echo 0) $d"; done | sort -n | tail -1)
        if [[ -n $big ]]; then
            mb=$(( ${big%% *} / 1048576 )); d=${big#* }
            echo 1 > "$d/cgroup.kill" 2>/dev/null
            echo "$(date '+%F %T') 可用 ${avail} MB < ${min} MB，杀掉 $(basename "$d")（${mb} MB）" >> "$log"
            sleep 3
        fi
    fi
    sleep 0.5
done
