# 源站进程的启停，供 run_all.sh、run_wild.sh、engines.sh source。
# 只结束本脚本起的源站，不按进程名一把杀（机器是共用的，别的会话可能也在跑同名的 origin）。
#   start_origin <二进制> <端口> <参数...>   后台启动，PID 记在 ORIGIN_PID
#   stop_origin                              结束 ORIGIN_PID：先 SIGTERM，5 秒内没退出再 SIGKILL
#   stop_stale_origin <二进制> <端口>        清掉上次残留的：按端口找监听者，可执行文件是这个二进制、
#                                           命令行里有这个端口，才结束它
ORIGIN_PID=

stop_pid() { # pid
    local pid=$1 state
    [ -n "$pid" ] || return 0
    kill -TERM "$pid" 2>/dev/null || return 0
    for _ in $(seq 1 50); do
        state=$(awk '{print $3}' "/proc/$pid/stat" 2>/dev/null) || break
        [ "$state" = Z ] && break
        sleep 0.1
    done
    if state=$(awk '{print $3}' "/proc/$pid/stat" 2>/dev/null) && [ "$state" != Z ]; then
        kill -KILL "$pid" 2>/dev/null
    fi
    wait "$pid" 2>/dev/null || true
}

stop_stale_origin() { # 二进制 端口
    local bin port=$2 pid
    bin=$(realpath "$1")
    for pid in $(ss -Hltnp "sport = :$port" 2>/dev/null | grep -o 'pid=[0-9]*' | cut -d= -f2 | sort -u); do
        [ "$(readlink -f "/proc/$pid/exe" 2>/dev/null)" = "$bin" ] || continue
        tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null | grep -qw -- "$port" || continue
        stop_pid "$pid"
    done
}

start_origin() { # 二进制 端口 参数...
    local bin=$1 port=$2; shift 2
    stop_stale_origin "$bin" "$port"
    "$bin" "$@" 2>/dev/null &
    ORIGIN_PID=$!
}

stop_origin() {
    stop_pid "$ORIGIN_PID"
    ORIGIN_PID=
}
