#!/usr/bin/env bash
# 检查 Linux gnu 产物要求的最高 glibc 版本不超过上限（默认 2.28）。
#   scripts/check_glibc.sh [--max X.Y] <文件>...
# 链接到新版 glibc 的符号会让产物在较老的系统上加载失败（GLIBC_x.y not found）；
# 发布用 manylinux_2_28 容器编译，这一步防止以后又在新系统上编出来。
# 强弱符号都算：保守起见，弱符号也不允许超过上限。
set -euo pipefail

MAX=2.28
if [ "${1:-}" = "--max" ]; then
    MAX=${2:?--max needs a version}
    shift 2
fi
if [ $# -lt 1 ]; then
    echo "usage: $0 [--max X.Y] <file>..." >&2
    exit 2
fi
command -v objdump >/dev/null || { echo "objdump not found (install binutils)" >&2; exit 2; }

status=0
for f in "$@"; do
    [ -f "$f" ] || { echo "$f: no such file" >&2; status=1; continue; }
    needed=$(objdump -T "$f" | grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/^GLIBC_//' | sort -uV)
    highest=$(printf '%s\n' "$needed" | tail -n 1)
    if [ -z "$highest" ]; then
        echo "$f: no GLIBC symbol versions found (not a glibc build?)" >&2
        status=1
        continue
    fi
    over=$(printf '%s\n%s\n' "$highest" "$MAX" | sort -V | tail -n 1)
    if [ "$over" != "$MAX" ]; then
        echo "$f: requires GLIBC_$highest, above the allowed GLIBC_$MAX" >&2
        objdump -T "$f" | grep "GLIBC_$highest" | awk '{print "    " $(NF-1) " " $NF}' | sort -u | head -n 10 >&2
        status=1
    else
        echo "$f: highest GLIBC_$highest (limit $MAX)"
    fi
done
exit $status
