#!/usr/bin/env bash
# 运行 Zig 绑定的测试（zig test）。zig 默认用 ~/mix-tools/toolchains/zig/bin/zig（ZIG 可覆盖）。
# 库位置：PAGESIFT_LIB（库文件完整路径），默认仓库里的 target/f/quick/libpagesift.so。
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
lib="${PAGESIFT_LIB:-$root/target/f/quick/libpagesift.so}"
cache="$(mktemp -d)"
trap 'rm -rf "$cache"' EXIT
cd "$here"
"${ZIG:-$HOME/mix-tools/toolchains/zig/bin/zig}" test test.zig -lc "$lib" \
  -rpath "$(dirname "$lib")" --cache-dir "$cache/c" --global-cache-dir "$cache/g"
