# libpagesift：Swift 绑定

`Pagesift.swift` 通过 `CPagesift/module.modulemap`（指向 `crates/pagesift-ffi/include/pagesift.h`）调用 C ABI。测试直接用 `swiftc`，不用 SwiftPM 和 XCTest。

## 测试

先在仓库根目录构建库，再跑测试：

```sh
CARGO_TARGET_DIR=target/f cargo build --profile quick -p pagesift-ffi
./bindings/swift/run_tests.sh
```

`run_tests.sh` 用官方 Linux 工具链（`~/mix-tools/toolchains/swift`，6.4.2），可用 `SWIFT_BIN` 指别处。测试 10 项，不联网。

## 用法

```swift
let md = Pagesift.convert("<h1>Hi</h1>")
let e = try Pagesift.Engine()
let doc = try e.parse(Array(csv.utf8), filename: "scores.csv")
```

返回 Document JSON 字符串，不解析。

## 库的查找

`swiftc` 的 `-I bindings/swift/CPagesift` 找头文件模块，库文件用 `PAGESIFT_LIB`（完整路径）传给链接器，默认仓库里的 `target/f/quick/libpagesift.so`，并写入 rpath。

## 安装

Swift Package Manager：`.package(url: "https://github.com/YOLOW92/pagesift.git", from: "<版本>")`，产品名 `Pagesift`。`Package.swift` 在仓库根，版本来自主仓库的标签 `v<版本>`，不用另外打标签。包里不带库：先安装 GitHub Release 里的 C 库，构建时加 `-Xlinker -L<库目录>`，运行时设 `LD_LIBRARY_PATH` / `DYLD_LIBRARY_PATH`。

打包脚本是 `package.sh`（`--ffi-dir` 用 Release 里解开的三个平台的 C 库，或 `--local-lib` 用本机一个库），它会在干净目录里从打好的包安装并跑一次 convert 和 parse。

## 发布时需要的产物

`libpagesift.so`（或 `.dylib`）、`pagesift.h` 和 `CPagesift/module.modulemap`、`Pagesift.swift`。
