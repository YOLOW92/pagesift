#!/usr/bin/env bash
# 整理出 pub.dev 的包目录（pagesift/）并在干净目录里实测。用法见 ../pkg-common.sh：
#   package.sh [--version V] (--ffi-dir DIR | --local-lib FILE) [--out DIR]
# 产物：--out/pagesift/（pubspec.yaml、lib/、native/<平台>/、README、LICENSE、NOTICE、CHANGELOG、example）和
#       （发布用 dart pub publish --force 在该目录里执行，压缩包由 pub 自己生成）
# 实测：dart pub publish --dry-run 校验包；再建一个只用 path 依赖的新项目，不设 PAGESIFT_LIB，跑 convert 和 parse。
# dart 默认用 ~/mix-tools/toolchains/dart-sdk/bin/dart（DART 可覆盖）。
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/../pkg-common.sh"
pkg_parse_args "$@"
pkg_prepare_libs
dart="${DART:-$HOME/mix-tools/toolchains/dart-sdk/bin/dart}"
export PUB_CACHE="$PKG_WORK/pub-cache"   # 不碰用户的 pub 缓存
export DART_SUPPRESS_ANALYTICS=true

p="$PKG_OUT/pagesift"; rm -rf "$p"; mkdir -p "$p/lib" "$p/example"
cp "$here/pagesift.dart" "$p/lib/pagesift.dart"
sed "s/@VERSION@/$PKG_VERSION/" "$here/pubspec.yaml.in" > "$p/pubspec.yaml"
for t in "${PKG_TARGETS_PRESENT[@]}"; do
  case "$t" in
    x86_64-unknown-linux-gnu) d=linux-x64 ;;
    aarch64-unknown-linux-gnu) d=linux-arm64 ;;
    aarch64-apple-darwin) d=macos-arm64 ;;
  esac
  mkdir -p "$p/native/$d"; cp "$PKG_LIBS/$t/lib/$(pkg_lib_name "$t")" "$p/native/$d/"
done
pkg_copy_legal "$p"; cp "$here/README.md" "$p/"
printf '## %s\n\n- Release for libpagesift %s. See https://github.com/YOLOW92/pagesift/releases\n' "$PKG_VERSION" "$PKG_VERSION" > "$p/CHANGELOG.md"
cat > "$p/example/example.dart" <<'DART'
import 'dart:convert';

import 'package:pagesift/pagesift.dart' as pagesift;

void main() {
  print(pagesift.convert('<h1>Hi</h1><p>World</p>'));
  final e = pagesift.Engine();
  print(e.parse(utf8.encode('name,score\nalice,1\n'), 'scores.csv'));
  e.close();
}
DART

# 1. pub 自己的发布前检查（不会上传）
cd "$p"
"$dart" pub get --offline >/dev/null
echo "== dart pub publish --dry-run"
dry="$("$dart" pub publish --dry-run 2>&1)" || { echo "$dry"; echo "dart pub publish --dry-run 失败" >&2; exit 1; }
echo "$dry" | sed -n '/Total compressed/,$p'
pkg_expect "$dry" "Package has 0 warnings" "pub 发布前检查无警告"
rm -rf "$p/.dart_tool" "$p/pubspec.lock"

# 2. 干净项目，path 依赖
c="$PKG_WORK/consumer"; mkdir -p "$c/bin"
cat > "$c/pubspec.yaml" <<YAML
name: consumer
publish_to: none
environment:
  sdk: ^3.3.0
dependencies:
  pagesift:
    path: $p
YAML
cat > "$c/bin/main.dart" <<'DART'
import 'dart:convert';
import 'package:pagesift/pagesift.dart' as pagesift;
void main() {
  print('version ${pagesift.version()}');
  print('CONVERT ${pagesift.convert('<h1>Hi</h1><p>World</p>')}');
  final e = pagesift.Engine();
  print('PARSE ${e.parse(utf8.encode('name,score\nalice,1\nbob,2\n'), 'scores.csv')}');
  e.close();
}
DART
cd "$c"
"$dart" pub get --offline >/dev/null
echo "== path 依赖实测（不设 PAGESIFT_LIB）"
res="$(env -u PAGESIFT_LIB -u LD_LIBRARY_PATH "$dart" run bin/main.dart)"
case "$res" in "version $PKG_VERSION"*) echo "  ok   库版本号等于包版本" ;; *) echo "  注意：库报告的版本和包版本不同：$(echo "$res" | head -1)" ;; esac
pkg_expect "$res" '"success":true' "convert/parse 成功"
pkg_expect "$res" "# Hi" "convert 输出标题"
pkg_expect "$res" "| alice | 1 |" "parse 输出表格行"
echo "package.sh dart 通过：$p"
