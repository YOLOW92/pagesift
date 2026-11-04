#!/usr/bin/env bash
# Swift 包在仓库根的 Package.swift，版本来自 Git 标签 v<版本>。这个脚本在干净目录里实测：
# 新建一个用 .package(path:) 引用仓库根的项目，`swift build`，C 库放在“已安装”的前缀（lib/、include/）里，
# 通过 -Xlinker -L 和运行时 LD_LIBRARY_PATH 找到，跑 convert 和 parse。不设 PAGESIFT_LIB。
# 用法见 ../pkg-common.sh：package.sh [--version V] (--ffi-dir DIR | --local-lib FILE)
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/../pkg-common.sh"
pkg_parse_args "$@"
pkg_prepare_libs
sb="${SWIFT_BIN:-$HOME/mix-tools/toolchains/swift/usr/bin}"
export PATH="$sb:$PATH"
echo "要打的标签：v$PKG_VERSION（主仓库的发布标签，不用另外打）"
# 把仓库根复制成不含 target/ 和 .git 的干净副本（只需要 Package.swift、bindings/swift、头文件）
r="$PKG_WORK/repo"; mkdir -p "$r/bindings" "$r/crates/pagesift-ffi"
cp "$PKG_ROOT/Package.swift" "$r/"; cp -r "$PKG_ROOT/bindings/swift" "$r/bindings/"
cp -r "$PKG_ROOT/crates/pagesift-ffi/include" "$r/crates/pagesift-ffi/"
c="$PKG_WORK/consumer"; mkdir -p "$c/Sources/app"; cd "$c"
cat > Package.swift <<SWIFT
// swift-tools-version:5.9
import PackageDescription
let package = Package(
    name: "app",
    dependencies: [.package(name: "pagesift", path: "$r")],
    targets: [.executableTarget(name: "app", dependencies: [.product(name: "Pagesift", package: "pagesift")], path: "Sources/app")]
)
SWIFT
cat > Sources/app/main.swift <<'SWIFT'
import Pagesift
print("version", Pagesift.version)
print("CONVERT", Pagesift.convert("<h1>Hi</h1><p>World</p>"))
let e = try Pagesift.Engine()
print("PARSE", try e.parse(Array("name,score\nalice,1\nbob,2\n".utf8), filename: "scores.csv"))
SWIFT
lib="$(dirname "$PKG_HOST_LIB")"
swift build -c release -Xlinker -L"$lib" 2>&1 | tail -5
res="$(env -u PAGESIFT_LIB LD_LIBRARY_PATH="$lib" .build/release/app)"
echo "$res" | cut -c1-200
pkg_expect "$res" '"success":true' "convert/parse 成功"
pkg_expect "$res" "# Hi" "convert 输出标题"
pkg_expect "$res" "| alice | 1 |" "parse 输出表格行"
echo "package.sh swift 通过"
