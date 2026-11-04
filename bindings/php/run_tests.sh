#!/usr/bin/env bash
# 运行 PHP 绑定的测试。需要带 ext-ffi 的 PHP；默认用 ~/mix-tools/toolchains/php-ffi/bin/php。
# 库位置：PAGESIFT_LIB（库文件完整路径），默认仓库里的 target/f/quick/libpagesift.so。
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
php="${PHP:-$HOME/mix-tools/toolchains/php-ffi/bin/php}"
export PAGESIFT_LIB="${PAGESIFT_LIB:-$(cd "$here/../.." && pwd)/target/f/quick/libpagesift.so}"
exec "$php" -d ffi.enable=1 "$here/test/test.php"
