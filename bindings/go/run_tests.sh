#!/usr/bin/env bash
# 运行 Go 绑定的测试。需要 go 与 cgo（gcc）。库目录用 PAGESIFT_LIB_DIR 指定，默认是仓库的 target/f/quick。
# go 默认用 ~/mix-tools/toolchains/go/bin（GO_BIN 可覆盖）。
# 环境里的 CGO_ENABLED/CC 可能被关掉或指向 /dev/null，这里显式设回来。
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
cd "$here"
export PATH="${GO_BIN:-$HOME/mix-tools/toolchains/go/bin}:$PATH"
export CGO_ENABLED=1
export CC="${CC_FOR_CGO:-/usr/bin/gcc}"
lib="${PAGESIFT_LIB_DIR:-$here/../../target/f/quick}"
export CGO_LDFLAGS="-L$lib -Wl,-rpath,$lib"
exec go test -count=1 -v ./...
