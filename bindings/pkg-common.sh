#!/usr/bin/env bash
# 各语言 package.sh 共用的小函数（被 source，不单独运行）。
#
# 用法（在各语言的 package.sh 里）：
#   . "$(dirname "$0")/../pkg-common.sh"
#   pkg_parse_args "$@"      # 解析 --version / --ffi-dir / --local-lib / --out
#   pkg_prepare_libs         # 得到 $PKG_LIBS：<目标三元组>/{lib,include}/ 的目录
#
# 两种库来源：
#   --ffi-dir DIR     DIR/<目标三元组>/lib/libpagesift.*，即 Release 里 pagesift-ffi-<tag>-<目标>.tar.gz 解开后的目录；
#                     三个平台都有就三个平台都打进包（CI 用这种）。
#   --local-lib FILE  只有本机平台的一个库文件（默认 PAGESIFT_LIB，再默认仓库 target/f/quick/libpagesift.so）。
#                     复制后用 strip 去掉调试信息，所以包大小接近 Release 的库；包里只有当前平台。
# 其他：
#   --version V       包版本（可带 v 前缀）；不给就取仓库根 Cargo.toml 的版本。
#   --out DIR         输出目录；不给就在临时目录建一个并打印路径。
set -euo pipefail

PKG_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PKG_TARGETS=(x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu aarch64-apple-darwin)

pkg_host_target() {
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) echo x86_64-unknown-linux-gnu ;;
    Linux-aarch64 | Linux-arm64) echo aarch64-unknown-linux-gnu ;;
    Darwin-arm64) echo aarch64-apple-darwin ;;
    *) echo "不支持的本机平台：$(uname -s)-$(uname -m)" >&2; return 1 ;;
  esac
}

# 目标三元组 → 库文件名
pkg_lib_name() {
  case "$1" in
    *apple-darwin) echo libpagesift.dylib ;;
    *) echo libpagesift.so ;;
  esac
}

pkg_parse_args() {
  PKG_VERSION="" PKG_FFI_DIR="" PKG_LOCAL_LIB="" PKG_OUT=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --version) PKG_VERSION="${2#v}"; shift 2 ;;
      --ffi-dir) PKG_FFI_DIR="$2"; shift 2 ;;
      --local-lib) PKG_LOCAL_LIB="$2"; shift 2 ;;
      --out) PKG_OUT="$2"; shift 2 ;;
      *) PKG_EXTRA+=("$1"); shift ;;
    esac
  done
  if [ -z "$PKG_VERSION" ]; then
    PKG_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$PKG_ROOT/Cargo.toml" | head -1)"
  fi
  [ -n "$PKG_VERSION" ] || { echo "取不到版本号" >&2; exit 1; }
  if [ -z "$PKG_OUT" ]; then
    PKG_OUT="$(mktemp -d)"
    echo "输出目录：$PKG_OUT"
  fi
  mkdir -p "$PKG_OUT"
  PKG_OUT="$(cd "$PKG_OUT" && pwd)"
  PKG_WORK="$(mktemp -d)"
  trap 'rm -rf "$PKG_WORK"' EXIT
}
PKG_EXTRA=()

# 准备 $PKG_LIBS/<目标>/lib/<库> 与 include/pagesift.h；同时设 $PKG_TARGETS_PRESENT。
pkg_prepare_libs() {
  PKG_LIBS="$PKG_WORK/libs"
  mkdir -p "$PKG_LIBS"
  PKG_TARGETS_PRESENT=()
  if [ -n "$PKG_FFI_DIR" ]; then
    local t
    for t in "${PKG_TARGETS[@]}"; do
      local f="$PKG_FFI_DIR/$t/lib/$(pkg_lib_name "$t")"
      if [ -f "$f" ]; then
        mkdir -p "$PKG_LIBS/$t/lib" "$PKG_LIBS/$t/include"
        cp "$f" "$PKG_LIBS/$t/lib/"
        cp "$PKG_ROOT/crates/pagesift-ffi/include/pagesift.h" "$PKG_LIBS/$t/include/"
        PKG_TARGETS_PRESENT+=("$t")
      fi
    done
    [ ${#PKG_TARGETS_PRESENT[@]} -gt 0 ] || { echo "$PKG_FFI_DIR 里一个平台的库都没有" >&2; exit 1; }
    if [ ${#PKG_TARGETS_PRESENT[@]} -ne ${#PKG_TARGETS[@]} ]; then
      echo "警告：只找到 ${#PKG_TARGETS_PRESENT[@]} 个平台：${PKG_TARGETS_PRESENT[*]}" >&2
    fi
  else
    local src="${PKG_LOCAL_LIB:-${PAGESIFT_LIB:-$PKG_ROOT/target/f/quick/libpagesift.so}}"
    [ -f "$src" ] || { echo "找不到本地库 $src（用 --local-lib 或 --ffi-dir 指定）" >&2; exit 1; }
    local t
    t="$(pkg_host_target)"
    mkdir -p "$PKG_LIBS/$t/lib" "$PKG_LIBS/$t/include"
    cp "$src" "$PKG_LIBS/$t/lib/$(pkg_lib_name "$t")"
    # 本地库是 quick 配置编的，带调试信息；去掉后大小才接近发布的库
    if [ "$(uname -s)" = Linux ]; then strip --strip-unneeded "$PKG_LIBS/$t/lib/$(pkg_lib_name "$t")" 2>/dev/null || true; fi
    cp "$PKG_ROOT/crates/pagesift-ffi/include/pagesift.h" "$PKG_LIBS/$t/include/"
    PKG_TARGETS_PRESENT=("$t")
  fi
  PKG_HOST="$(pkg_host_target)"
  [ -f "$PKG_LIBS/$PKG_HOST/lib/$(pkg_lib_name "$PKG_HOST")" ] || {
    echo "没有本机平台（$PKG_HOST）的库，无法做安装实测" >&2; exit 1; }
  PKG_HOST_LIB="$PKG_LIBS/$PKG_HOST/lib/$(pkg_lib_name "$PKG_HOST")"
}

# 复制许可证文件到目录
pkg_copy_legal() {
  cp "$PKG_ROOT/LICENSE" "$PKG_ROOT/NOTICE" "$1/"
}

# 检查输出里有这段字符串，否则失败
pkg_expect() { # 输出文本 子串 说明
  case "$1" in
    *"$2"*) echo "  ok   $3" ;;
    *) echo "  FAIL $3（没找到：$2）" >&2; echo "---- 实际输出 ----" >&2; echo "$1" | head -c 600 >&2; exit 1 ;;
  esac
}

# 实测用的 CSV
PKG_CSV=$'name,score\nalice,1\nbob,2\n'
