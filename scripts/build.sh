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
LOCKS=${BENCH_LOCK_DIR:-tasks/0.80}
jobs=$(scripts/jobs.sh)
dir=$(realpath -m "${CARGO_TARGET_DIR:-target}")
slot=$(printf '%s' "$dir" | sha1sum | cut -c1-12)

# 测速要先拿 bench-gate.lock 的排它锁再等 bench.lock：编译进门时短暂拿一下闸门的共享锁，测速一申请，
# 新来的编译就停在门外，正在跑的编译结束后测速就能开始（flock 本身不保证排它锁不被共享锁饿死）
# 先排编译目录，轮到了再进闸门：在目录锁上排队的编译不占 bench.lock，测速只等真正在跑的那几个
exec 9<>"$LOCKS/build-dir-$slot.lock"
flock 9
exec 7<>"$LOCKS/bench-gate.lock"
flock -s 7
exec 8<>"$LOCKS/bench.lock"
flock -s 8
exec 7>&-
exec env CARGO_BUILD_JOBS="$jobs" "$@"
