#!/usr/bin/env bash
# 编译和测试的排队入口：多个会话同时开发时用它，不要直接 flock bench.lock。
#
#   scripts/build.sh cargo test --profile release-fast -q -p pagesift-net
#
# - 和测速互斥：拿 bench.lock 的共享锁。测速脚本拿的是排它锁（先拿闸门，见下），测速期间不会有编译。
# - cargo 对同一个编译目录（CARGO_TARGET_DIR，默认 target/）同一时间只让一个编译进去，
#   其余的在 cargo 自己的锁上干等。所以按编译目录排队：每个目录一把锁，拿到的编译用满
#   scripts/jobs.sh 算出的并行度（按内存算）。用不同 CARGO_TARGET_DIR 的编译互不等待。
set -euo pipefail
cd "$(dirname "$0")/.."
if [ -n "${BENCH_LOCK_DIR:-}" ]; then
    LOCKS=$BENCH_LOCK_DIR
elif [ -d tasks/0.80 ]; then
    LOCKS=tasks/0.80
else
    LOCKS="${TMPDIR:-/tmp}/pagesift-build-locks"
fi
mkdir -p "$LOCKS"
if [[ " $* " =~ (^|[[:space:]\'\"])(--release|--profile(=|[[:space:]])release)([[:space:]\'\"]|$) ]]; then
    # [profile.release] 启用了 lto = "fat"，单链接进程峰值内存高，严格限制并发防 OOM
    jobs=$(scripts/jobs.sh 2800 2560)
elif [[ " $* " =~ (^|[[:space:]\'\"])--profile(=|[[:space:]])release-fast([[:space:]\'\"]|$) ]]; then
    # [profile.release-fast] 使用 thin LTO + 16 单元，适度限流兼顾速度与内存
    jobs=$(scripts/jobs.sh 1400 2048)
else
    # --profile quick / check / clippy 无 fat LTO，可安全提高并发
    jobs=$(scripts/jobs.sh 1200 2048)
fi
# 先对真实路径拿目录锁再清理与执行，避免跨会话并发删缓存，也避免 target 为软链接时 find 不穿透
dir=$(realpath -m "${CARGO_TARGET_DIR:-target}")
slot=$(printf '%s' "$dir" | sha1sum | cut -c1-12)

# 测速要先拿 bench-gate.lock 的排它锁再等 bench.lock：编译进门时短暂拿一下闸门的共享锁，测速一申请，
# 新来的编译就停在门外，正在跑的编译结束后测速就能开始（flock 本身不保证排它锁不被共享锁饿死）
# 先排编译目录，轮到了再进闸门：在目录锁上排队的编译不占 bench.lock，测速只等真正在跑的那几个
exec 9<>"$LOCKS/build-dir-$slot.lock"
flock 9

prune_target() {
    local phase=$1
    shift
    [ -d "$dir" ] || return 0
    # 清理跨版本/旧哈希残留、临时目标文件与膨胀产物，避免 /mnt/c 编译目录无限增长
    local free_kib target_kib=0
    free_kib=$(df --output=avail -k "$dir" 2>/dev/null | tail -1 || echo 0)
    if [ "$phase" = "post" ]; then
        find "$dir" -maxdepth 4 \( -path '*/deps/*' -o -path '*/examples/*' \) -type f -name '*.rcgu.o*' -delete 2>/dev/null || true
        if [[ " $* " != *" --example"* ]]; then
            find "$dir" -maxdepth 4 -path '*/examples/*' -type f -perm -111 -delete 2>/dev/null || true
        fi
        target_kib=$(du -sk "$dir" 2>/dev/null | cut -f1 || echo 0)
    fi
    if [ "${free_kib:-0}" -lt $((80 * 1024 * 1024)) ] || [ "${target_kib:-0}" -gt $((18 * 1024 * 1024)) ]; then
        find "$dir" -maxdepth 4 -type d -name incremental -prune -exec rm -rf {} + 2>/dev/null || true
        find "$dir" -maxdepth 4 -path '*/deps/*' -type f -mmin +30 \
            -perm -111 ! -name '*.so' ! -name '*.dylib' ! -name '*.dll' -delete 2>/dev/null || true
        find "$dir" -maxdepth 4 -path '*/deps/*' -type f -mmin +360 -delete 2>/dev/null || true
    else
        find "$dir" -mindepth 3 -maxdepth 4 -path '*/incremental/*' -type d -mmin +360 -prune -exec rm -rf {} + 2>/dev/null || true
        find "$dir" -maxdepth 4 \( -path '*/deps/*' -o -path '*/examples/*' \) -type f -mmin +60 \
            -perm -111 ! -name '*.so' ! -name '*.dylib' ! -name '*.dll' -delete 2>/dev/null || true
        find "$dir" -maxdepth 4 -path '*/deps/*' -type f -mmin +1440 -delete 2>/dev/null || true
    fi
}

prune_target pre "$@"

exec 7<>"$LOCKS/bench-gate.lock"
flock -s 7
exec 8<>"$LOCKS/bench.lock"
flock -s 8
exec 7>&-

rc=0
env CARGO_BUILD_JOBS="$jobs" "$@" || rc=$?
exec 8>&-

prune_target post "$@"
exit "$rc"
