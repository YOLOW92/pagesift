# libpagesift：Go（cgo）绑定

`pagesift.go` 把 C ABI 包成 Go 的 `Document`、`Error` 和 `Engine`。需要 cgo（gcc）。

## 测试

先在仓库根目录构建库，再跑测试：

```sh
CARGO_TARGET_DIR=target/f cargo build --profile quick -p pagesift-ffi
./bindings/go/run_tests.sh
```

测试 7 项，不联网。`run_tests.sh` 显式打开 `CGO_ENABLED=1`，并把 `CC` 设为 `/usr/bin/gcc`
（本机的 Go 环境里这两项默认是关的/指向 /dev/null）。

## 库的位置

从 Release 下载 `pagesift-ffi-<版本>-<平台>.tar.gz`，解压后编译时告诉 cgo：

```sh
CGO_LDFLAGS="-L<目录>/lib -Wl,-rpath,<目录>/lib" go build ./...
```

## 用法

```go
import pagesift "github.com/YOLOW92/pagesift/bindings/go"

md, err := pagesift.Convert("<h1>Hi</h1>", nil)

e, err := pagesift.NewEngine(nil)
defer e.Close()
doc, err := e.Parse(csvBytes, "scores.csv", nil)
fmt.Println(*doc.Markdown)
```

返回值是 `*Document` 和 `error`。`success` 为 false 时 `error` 是 `*pagesift.Error`，带 `Code` 和 `Message`；
完整 JSON 在 `doc.Raw` 里。

## 库的查找

`pagesift.go` 顶部的 `#cgo LDFLAGS` 指向仓库里的 `target/f/quick/libpagesift.so`，并写入 rpath。
要用别处的库，改这一行（指向库目录），或在自己的项目里把它换成 `pkg-config`。

发布时需要 `libpagesift.so`（或 `.dylib`、`.dll`）和 `pagesift.h`。

## 安装

不需要包仓库：`go get github.com/YOLOW92/pagesift/bindings/go@v<版本>`。Go 子目录模块的版本来自 Git 标签 `bindings/go/v<版本>`，发版时要打这个标签（模块路径不变）。先安装 GitHub Release 里的 C 库，构建时用 `CGO_LDFLAGS="-L<库目录>"`（或把库放进系统库路径），运行时库要在动态链接器的路径上。cgo 是链接时绑定，没有运行时的 `PAGESIFT_LIB`。`package.sh` 在干净目录里用 replace 引用本模块并跑一次 convert 和 parse。
