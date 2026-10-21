#!/usr/bin/env bash
# 按物理核绑核并行：从标准输入每行读一条命令，每条命令独占一个物理核（只用每个物理核的第一个
# 逻辑 CPU，超线程兄弟保持空闲）。核用文件锁占用（/tmp/pagesift-core-<cpu>.lock），所以同时运行的
# 多个 pinned.sh（不同领域的评测）共用同一组物理核：任何时刻一个物理核上只有一条命令，有活就不空着。
# 单线程计时在这种条件下对所有工具相同；需要全核的吞吐测试不要和它同时跑。
#   printf '%s\n' "cmd1 ..." "cmd2 ..." | bench/fresh/pinned.sh
#   bench/fresh/pinned.sh --cores     # 只打印用到的逻辑 CPU
set -uo pipefail
mapfile -t CORES < <(lscpu -p=CPU,CORE | grep -v '^#' | sort -t, -k2,2n -k1,1n | awk -F, '!seen[$2]++ {print $1}')
# 留出最后 PIN_RESERVE 个物理核（默认 1 个，连同它的超线程）给不计时的活：调度、评分、统计。
# 用 `bench/fresh/pinned.sh --reserved` 拿到这些逻辑 CPU，用 taskset 把不计时的进程限制在上面。
RESERVE=${PIN_RESERVE:-1}
LAST=$(( ${#CORES[@]} - RESERVE ))
RESERVED=$(lscpu -p=CPU,CORE | grep -v '^#' | awk -F, -v keep="$(printf '%s,' "${CORES[@]:LAST}")" \
    'BEGIN{n=split(keep,k,","); for(i=1;i<=n;i++) if(k[i]!="") want[k[i]]=1} {cpu[$1]=$2} END{for(c in cpu) if(cpu[c] in core) print c} {if($1 in want) core[$2]=1}' | sort -n | paste -sd, -)
CORES=("${CORES[@]:0:LAST}")
# PIN_HT=1：把超线程也当作独立的 CPU 用（16 线程的机器上用 14 个逻辑 CPU，保留的物理核除外），更快，
# 但同一物理核上两个任务会互相拖慢，单页耗时比一核一任务时偏高；结果里要注明这个条件
if [ "${PIN_HT:-0}" = 1 ]; then
    mapfile -t CORES < <(lscpu -p=CPU | grep -v '^#' | grep -v -x -F -f <(tr ',' '\n' <<< "$RESERVED"))
fi
case "${1:-}" in
    --cores) echo "计时用的逻辑 CPU：${CORES[*]}；留给不计时任务的：$RESERVED"; exit 0 ;;
    --reserved) echo "$RESERVED"; exit 0 ;;
esac
# 计时任务只用分到的那个核：Python 里的 onnxruntime 由 one_core.py 限成单线程，BLAS/OpenMP 线程池设成 1
export PAGESIFT_ONE_CORE=1 OMP_NUM_THREADS=1 OPENBLAS_NUM_THREADS=1 MKL_NUM_THREADS=1 NUMEXPR_NUM_THREADS=1 \
    RAYON_NUM_THREADS=1 TOKIO_WORKER_THREADS=1 UV_THREADPOOL_SIZE=1
pids=()
# 占一个空闲的核：拿到锁的文件描述符留给子进程，子进程结束时锁自动释放
start() {
    local cmd=$1 c fd
    while :; do
        # 内存不够时先不开新任务（每个任务可能占 1 GB 以上），等前面的跑完
        if [ "$(awk '/^MemAvailable:/ {print int($2/1024)}' /proc/meminfo)" -lt "${PIN_MIN_FREE_MB:-1500}" ]; then
            sleep 1
            continue
        fi
        for c in "${CORES[@]}"; do
            exec {fd}>"/tmp/pagesift-core-$c.lock"
            if flock -n "$fd"; then
                taskset -c "$c" bash -c "$cmd" &
                pids+=($!)
                exec {fd}>&-
                return
            fi
            exec {fd}>&-
        done
        sleep 0.3
    done
}
while IFS= read -r cmd; do
    [ -n "$cmd" ] && start "$cmd"
done
status=0
for p in "${pids[@]}"; do wait "$p" || status=1; done
exit $status
