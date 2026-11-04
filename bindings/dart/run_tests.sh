#!/usr/bin/env bash
# 运行 Dart 绑定的测试。dart 默认用 ~/mix-tools/toolchains/dart-sdk/bin/dart（DART 可覆盖）。
# 库位置：PAGESIFT_LIB（库文件完整路径），默认仓库里的 target/f/quick/libpagesift.so。
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
export PAGESIFT_LIB="${PAGESIFT_LIB:-$(cd "$here/../.." && pwd)/target/f/quick/libpagesift.so}"
exec "${DART:-$HOME/mix-tools/toolchains/dart-sdk/bin/dart}" run "$here/test/test.dart"
