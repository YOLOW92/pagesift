#!/usr/bin/env bash
# Go 没有包仓库：用户 `go get github.com/YOLOW92/pagesift/bindings/go@v<版本>`，版本来自 Git 标签 bindings/go/v<版本>（要打这个标签）。
# 这个脚本在干净目录里检查并实测：模块目录带 LICENSE、NOTICE（和仓库根的一致）；新建一个用 replace 指向它的项目，
# 用 CGO_LDFLAGS 指到“已安装的 C 库”所在目录（cgo 是链接时绑定，没有运行时的 PAGESIFT_LIB），跑 convert 和 parse。
# 用法见 ../pkg-common.sh：package.sh [--version V] (--ffi-dir DIR | --local-lib FILE)
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/../pkg-common.sh"
pkg_parse_args "$@"
pkg_prepare_libs
export PATH="${GO_BIN:-$HOME/mix-tools/toolchains/go/bin}:$PATH"
export CGO_ENABLED=1 CC="${CC_FOR_CGO:-/usr/bin/gcc}" GOFLAGS=-mod=mod GOPROXY=off GOTOOLCHAIN=local
cmp "$here/LICENSE" "$PKG_ROOT/LICENSE"; cmp "$here/NOTICE" "$PKG_ROOT/NOTICE"
cmp "$here/pagesift.h" "$PKG_ROOT/crates/pagesift-ffi/include/pagesift.h"
echo "  ok   LICENSE、NOTICE、pagesift.h 和仓库一致"
echo "要打的标签：bindings/go/v$PKG_VERSION"
m="$PKG_WORK/module"; mkdir -p "$m"; (cd "$here" && tar --exclude=package.sh --exclude=run_tests.sh -cf - .) | (cd "$m" && tar xf -)
c="$PKG_WORK/consumer"; mkdir -p "$c"; cd "$c"
cat > go.mod <<GO
module example.com/consumer

go 1.22

require github.com/YOLOW92/pagesift/bindings/go v0.0.0

replace github.com/YOLOW92/pagesift/bindings/go => $m
GO
cat > main.go <<'GO'
package main

import (
	"fmt"

	pagesift "github.com/YOLOW92/pagesift/bindings/go"
)

func main() {
	fmt.Println("version", pagesift.Version())
	d, err := pagesift.Convert("<h1>Hi</h1><p>World</p>", nil)
	fmt.Println("CONVERT", d.Success, *d.Markdown, err)
	e, err := pagesift.NewEngine(nil)
	if err != nil {
		panic(err)
	}
	defer e.Close()
	p, err := e.Parse([]byte("name,score\nalice,1\nbob,2\n"), "scores.csv", nil)
	fmt.Println("PARSE", p.Success, *p.Markdown, err)
}
GO
lib="$(dirname "$PKG_HOST_LIB")"
res="$(env -u PAGESIFT_LIB CGO_LDFLAGS="-L$lib -Wl,-rpath,$lib" go run . 2>&1)" || { echo "$res"; exit 1; }
echo "$res"
pkg_expect "$res" "CONVERT true # Hi" "convert 成功"
pkg_expect "$res" "PARSE true" "parse 成功"
pkg_expect "$res" "| alice | 1 |" "parse 输出表格行"
echo "package.sh go 通过"
