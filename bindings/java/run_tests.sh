#!/usr/bin/env bash
# 编译并运行 Java 绑定的测试（JDK 22+ 的 FFM，无第三方依赖）。
# JAVA_HOME 没设时用 ~/mix-tools/toolchains/jdk-25。库位置：PAGESIFT_LIB（库文件完整路径），默认仓库里的 target/f/quick/libpagesift.so。
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
jdk="${JAVA_HOME:-$HOME/mix-tools/toolchains/jdk-25}"
out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
"$jdk/bin/javac" --release 25 -d "$out" "$here"/src/dev/pagesift/*.java "$here"/test/dev/pagesift/*.java
root="$(cd "$here/../.." && pwd)"
lib="${PAGESIFT_LIB:-$root/target/f/quick/libpagesift.so}"
"$jdk/bin/java" --enable-native-access=ALL-UNNAMED -Dpagesift.lib="$lib" \
  -cp "$out" dev.pagesift.PagesiftTest
