#!/usr/bin/env bash
# 交叉"类型检查"用的假 C 编译器：只按 -o 生成空的目标文件并返回成功。
# `cargo check` 不链接，构建脚本编出来的 C 目标文件用不到；本机没有目标平台的 C 工具链时，
# 用它让 zstd-sys、blake3 这类构建脚本跑完，Rust 代码照常做完整的平台类型检查（scripts/ci.sh）。
# 作归档工具（AR_<目标>）调用时：`ar cq <库> <目标文件…>`，只生成空的库文件
if [ "${1:-}" = "cq" ] || [ "${1:-}" = "crs" ] || [ "${1:-}" = "rcs" ] || [ "${1:-}" = "s" ]; then
    [ -n "${2:-}" ] && : > "$2"
    exit 0
fi
out=
while [ $# -gt 0 ]; do
    case "$1" in
        -o) out=$2; shift ;;
        -o*) out=${1#-o} ;;
    esac
    shift
done
[ -n "$out" ] && : > "$out"
exit 0
