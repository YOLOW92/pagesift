#!/usr/bin/env bash
# 第 2 版新加的 Python 工具：按 locks/py/<工具>.in 生成带哈希的锁（第一次），再装进 $MIX/py/<工具>。
#   benchmarks/mix/v2/install_py.sh <工具> [额外的 uv 参数...]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
t=$1; shift
mix=${MIX_TOOLS:-$HOME/mix-tools}
lock=$here/locks/py/$t.txt
if [[ ! -f $lock ]]; then
    uv pip compile -q --generate-hashes --python-version 3.12 --python-platform x86_64-manylinux_2_28 "$@" \
        "$here/locks/py/$t.in" -o "$lock"
fi
uv venv -q -p 3.12 "$mix/py/$t"
VIRTUAL_ENV=$mix/py/$t uv pip sync -q --require-hashes "$@" "$lock"
echo "$t 装好：$(du -sh "$mix/py/$t" | cut -f1)，$(grep -c '==' "$lock") 个包"
