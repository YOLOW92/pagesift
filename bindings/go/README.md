# pagesift for Go

A cgo binding of [pagesift](https://github.com/YOLOW92/pagesift) with `Document`, `Error` and `Engine` types. It needs cgo and a C compiler.

## Install

```sh
go get github.com/YOLOW92/pagesift/bindings/go@v1.1.3
```

The package does not include the library. Download `pagesift-ffi-<tag>-<target>.tar.gz` from [GitHub Releases](https://github.com/YOLOW92/pagesift/releases) (Linux x86_64, Linux aarch64, macOS arm64), or build it with `cargo build --release -p pagesift-ffi`. Then tell cgo where it is, or put it on the system library path:

```sh
CGO_LDFLAGS="-L<dir>/lib -Wl,-rpath,<dir>/lib" go build ./...
```

cgo links at build time, so `PAGESIFT_LIB` does not apply. At run time the library must be on the dynamic linker's path.

## Usage

```go
import pagesift "github.com/YOLOW92/pagesift/bindings/go"

md, err := pagesift.Convert("<h1>Hi</h1>", nil)

e, err := pagesift.NewEngine(nil)
defer e.Close()
doc, err := e.Parse(csvBytes, "scores.csv", nil)
fmt.Println(*doc.Markdown)
```

Calls return `*Document` and `error`. When `success` is false, `error` is a `*pagesift.Error` with `Code` and `Message`. The full JSON is in `doc.Raw`.

## Tests

From the repository root:

```sh
CARGO_TARGET_DIR=target/f cargo build --profile quick -p pagesift-ffi
./bindings/go/run_tests.sh
```

The tests do not use the network. Set `PAGESIFT_LIB_DIR` to use a library elsewhere.

## License

AGPL-3.0-only, with commercial licensing available. See the [main repository](https://github.com/YOLOW92/pagesift).
