#!/usr/bin/env bash
# 打 Maven Central 的包并在干净目录里实测。用法见 ../pkg-common.sh：
#   package.sh [--version V] (--ffi-dir DIR | --local-lib FILE) [--out DIR]
# 产物（在 --out 下）：
#   maven/io/github/yolow92/pagesift/<版本>/    jar、sources、javadoc、pom（还没签名）
#   central-bundle-unsigned.zip                 带 md5/sha1 的上传包（没有 .asc；发布时由 bundle.sh --sign 重做）
# 实测：只用 jar 放进 classpath（不设 PAGESIFT_LIB、不设 -Dpagesift.lib），从 jar 里解压库后跑测试。
# 需要 JDK 22+（默认 ~/mix-tools/toolchains/jdk-25，JAVA_HOME 可覆盖）。
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/../pkg-common.sh"
pkg_parse_args "$@"
pkg_prepare_libs
jdk="${JAVA_HOME:-$HOME/mix-tools/toolchains/jdk-25}"
export PATH="$jdk/bin:$PATH"
GAV=io/github/yolow92/pagesift/$PKG_VERSION
M="$PKG_OUT/maven/$GAV"
rm -rf "$PKG_OUT/maven"; mkdir -p "$M"
w="$PKG_WORK/java"; mkdir -p "$w/classes" "$w/res/META-INF"

# 1. 编译（--release 22：FFM 从 22 起是正式版）
javac --release 22 -encoding UTF-8 -d "$w/classes" "$here"/src/dev/pagesift/*.java

# 2. 资源：三个平台的库 + 许可证
for t in "${PKG_TARGETS_PRESENT[@]}"; do
  case "$t" in
    x86_64-unknown-linux-gnu) d=linux-x86_64 ;;
    aarch64-unknown-linux-gnu) d=linux-aarch64 ;;
    aarch64-apple-darwin) d=macos-aarch64 ;;
  esac
  mkdir -p "$w/res/META-INF/native/$d"
  cp "$PKG_LIBS/$t/lib/$(pkg_lib_name "$t")" "$w/res/META-INF/native/$d/"
done
cp "$PKG_ROOT/LICENSE" "$PKG_ROOT/NOTICE" "$w/res/META-INF/"
printf 'Automatic-Module-Name: dev.pagesift\nImplementation-Title: pagesift\nImplementation-Version: %s\n' "$PKG_VERSION" > "$w/manifest.txt"

# 3. 三个 jar（--date 固定时间戳，同样的输入得到同样的 jar）
jar --create --file "$M/pagesift-$PKG_VERSION.jar" --manifest "$w/manifest.txt" --date 2026-01-01T00:00:00Z \
  -C "$w/classes" . -C "$w/res" .
mkdir -p "$w/src/META-INF"; cp -r "$here/src/dev" "$w/src/"; cp "$PKG_ROOT/LICENSE" "$PKG_ROOT/NOTICE" "$w/src/META-INF/"
jar --create --file "$M/pagesift-$PKG_VERSION-sources.jar" --date 2026-01-01T00:00:00Z -C "$w/src" .
mkdir -p "$w/doc"
javadoc -quiet -Xdoclint:none -encoding UTF-8 -docencoding UTF-8 -charset UTF-8 --release 22 -d "$w/doc" "$here"/src/dev/pagesift/*.java
jar --create --file "$M/pagesift-$PKG_VERSION-javadoc.jar" --date 2026-01-01T00:00:00Z -C "$w/doc" .
sed "s/@VERSION@/$PKG_VERSION/g" "$here/pom.xml.in" > "$M/pagesift-$PKG_VERSION.pom"

# 4. 上传包（未签名）
bash "$here/bundle.sh" "$PKG_OUT/maven" "$PKG_OUT/central-bundle-unsigned.zip"
ls -l "$M"

# 5. 实测：干净目录，只有 jar 在 classpath 上
t="$PKG_WORK/consumer"; mkdir -p "$t/out"
javac --release 22 -encoding UTF-8 -cp "$M/pagesift-$PKG_VERSION.jar" -d "$t/out" "$here"/test/dev/pagesift/*.java
echo "== 从 jar 安装实测（不设 PAGESIFT_LIB）"
cd "$t"
res="$(env -u PAGESIFT_LIB -u LD_LIBRARY_PATH java --enable-native-access=ALL-UNNAMED \
  -cp "$M/pagesift-$PKG_VERSION.jar:$t/out" dev.pagesift.PagesiftTest)"
echo "$res" | tail -4
pkg_expect "$res" ", 0 failed" "Java 测试无失败"
echo "package.sh java 通过：$M"
