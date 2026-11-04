#!/usr/bin/env bash
# Kotlin 没有单独的包：直接用 Maven 坐标 io.github.yolow92:pagesift 的 Java 包（见 ../java）。
# 这个脚本先用 ../java/package.sh 打出 jar，再在干净目录里用 kotlinc 只对着这个 jar 编译并运行 Kotlin 测试，
# 证明 Kotlin 项目只加这一个依赖就能用（不设 PAGESIFT_LIB，库从 jar 里解压）。
# 参数和 ../java/package.sh 相同。
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
kt="${KOTLIN_HOME:-$HOME/mix-tools/toolchains/kotlin}"
export JAVA_HOME="${JAVA_HOME:-$HOME/mix-tools/toolchains/jdk-25}"
export CONDA_PREFIX="$kt"
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
out=""; args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do [ "${args[i]}" = "--out" ] && out="${args[i+1]}"; done
if [ -z "$out" ]; then out="$work/java-out"; args+=(--out "$out"); fi
bash "$here/../java/package.sh" "${args[@]}"
jar="$(ls "$out"/maven/io/github/yolow92/pagesift/*/pagesift-*[0-9].jar)"
mkdir -p "$work/kt" && cd "$work"
"$kt/libexec/kotlin/bin/kotlinc" -nowarn -cp "$jar" -d "$work/kt" "$here/test/PagesiftTest.kt"
stdlib="$kt/libexec/kotlin/lib/kotlin-stdlib.jar"
echo "== Kotlin 只依赖 jar 的实测"
res="$(env -u PAGESIFT_LIB -u LD_LIBRARY_PATH "$JAVA_HOME/bin/java" --enable-native-access=ALL-UNNAMED \
  -cp "$jar:$work/kt:$stdlib" PagesiftTestKt)"
echo "$res" | tail -3
case "$res" in *", 0 failed"*) echo "package.sh kotlin 通过" ;; *) echo "Kotlin 测试有失败" >&2; exit 1 ;; esac
