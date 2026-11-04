#!/usr/bin/env bash
# 编译并运行 Swift 绑定的测试（直接 swiftc，不用 SwiftPM / XCTest）。
# swiftc 默认用 ~/mix-tools/toolchains/swift/usr/bin（SWIFT_BIN 可覆盖）。
# 库位置：PAGESIFT_LIB（库文件完整路径），默认仓库里的 target/f/quick/libpagesift.so。
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
lib="${PAGESIFT_LIB:-$root/target/f/quick/libpagesift.so}"
swiftbin="${SWIFT_BIN:-$HOME/mix-tools/toolchains/swift/usr/bin}"
out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
# main.swift 必须是入口文件，库源文件和它一起编成一个模块。
"$swiftbin/swiftc" -module-name PagesiftTest -I "$here/CPagesift" \
  "$here/Pagesift.swift" "$here/test/main.swift" \
  "$lib" -Xlinker -rpath -Xlinker "$(dirname "$lib")" -o "$out/test"
"$out/test"
