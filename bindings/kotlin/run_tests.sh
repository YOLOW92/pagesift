#!/usr/bin/env bash
# 编译并运行 Kotlin 绑定的测试：Kotlin 直接用 ../java 的封装（JDK 22+ 的 FFM）。
# 工具链：KOTLIN_HOME（默认 ~/mix-tools/toolchains/kotlin，conda-forge 的 kotlin）、JAVA_HOME（默认 jdk-25）。
# 库位置：PAGESIFT_LIB（库文件完整路径），默认仓库里的 target/f/quick/libpagesift.so。
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
kt="${KOTLIN_HOME:-$HOME/mix-tools/toolchains/kotlin}"
export JAVA_HOME="${JAVA_HOME:-$HOME/mix-tools/toolchains/jdk-25}"
export CONDA_PREFIX="$kt"   # conda 版 kotlinc 的启动脚本要用
out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
"$JAVA_HOME/bin/javac" --release 25 -d "$out/java" "$here"/../java/src/dev/pagesift/*.java
"$kt/libexec/kotlin/bin/kotlinc" -nowarn -cp "$out/java" -d "$out/kt" "$here/test/PagesiftTest.kt"
lib="${PAGESIFT_LIB:-$root/target/f/quick/libpagesift.so}"
stdlib="$(ls "$kt"/libexec/kotlin/lib/kotlin-stdlib.jar)"
"$JAVA_HOME/bin/java" --enable-native-access=ALL-UNNAMED -Dpagesift.lib="$lib" \
  -cp "$out/java:$out/kt:$stdlib" PagesiftTestKt
