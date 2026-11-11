# pagesift for Swift

A Swift binding of [pagesift](https://github.com/YOLOW92/pagesift) over its C ABI.

## Install

```swift
.package(url: "https://github.com/YOLOW92/pagesift.git", from: "1.2.5")
```

The product is `Pagesift`.

The package does not include the library. Download `pagesift-ffi-<tag>-<target>.tar.gz` from [GitHub Releases](https://github.com/YOLOW92/pagesift/releases) (Linux x86_64, Linux aarch64, macOS arm64), or build it with `cargo build --release -p pagesift-ffi`. Build with `-Xlinker -L<lib dir>`, and at run time set `LD_LIBRARY_PATH` or `DYLD_LIBRARY_PATH`.

## Usage

```swift
let md = Pagesift.convert("<h1>Hi</h1>")
let e = try Pagesift.Engine()
let doc = try e.parse(Array(csv.utf8), filename: "scores.csv")
```

Functions return the Document as a JSON string and do not parse it.

## Tests

From the repository root:

```sh
CARGO_TARGET_DIR=target/f cargo build --profile quick -p pagesift-ffi
./bindings/swift/run_tests.sh
```

The tests do not use the network. Set `SWIFT_BIN` to choose the toolchain.

## License

AGPL-3.0-only, with commercial licensing available. See the [main repository](https://github.com/YOLOW92/pagesift).
