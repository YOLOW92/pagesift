#!/usr/bin/env bash
# 编译并运行 C 绑定的测试。
# 库位置：PAGESIFT_LIB_DIR（含 libpagesift.so/.a），默认仓库里的 target/f/quick。
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
lib_dir="${PAGESIFT_LIB_DIR:-$root/target/f/quick}"
inc_dir="$root/crates/pagesift-ffi/include"
out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
cc="${CC:-cc}"
"$cc" -std=c11 -Wall -Wextra -I"$inc_dir" "$here/test/test_pagesift.c" \
  -L"$lib_dir" -lpagesift -Wl,-rpath,"$lib_dir" -o "$out/test_pagesift"
"$out/test_pagesift"
