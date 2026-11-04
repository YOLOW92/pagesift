#!/usr/bin/env bash
# 运行 Ruby 绑定的测试（fiddle 和 minitest 都是标准库或其自带的 gem）。
# ruby 在 ~/mix-tools/toolchains/ruby；库位置用 PAGESIFT_LIB，默认仓库里的 target/f/quick。
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
ruby="${RUBY:-$HOME/mix-tools/toolchains/ruby/bin/ruby}"
export PAGESIFT_LIB="${PAGESIFT_LIB:-$(cd "$here/../.." && pwd)/target/f/quick/libpagesift.so}"
exec "$ruby" -I"$here" "$here/test/test_pagesift.rb"
