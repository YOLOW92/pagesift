#!/usr/bin/env bash
# 运行 LuaJIT 绑定的测试。luajit 默认用 ~/mix-tools/toolchains/luajit/bin/luajit（LUAJIT 可覆盖）。
# 库位置：PAGESIFT_LIB（库文件完整路径），默认仓库里的 target/f/quick/libpagesift.so。
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
export PAGESIFT_LIB="${PAGESIFT_LIB:-$(cd "$here/../.." && pwd)/target/f/quick/libpagesift.so}"
exec "${LUAJIT:-$HOME/mix-tools/toolchains/luajit/bin/luajit}" "$here/test/test.lua"
