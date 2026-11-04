# libpagesift：Zig 绑定

`pagesift.zig` 直接声明 C ABI（不用 `@cImport`，所以不受 Zig 版本间 C 导入方式变化的影响），不依赖 std 以外的东西。结果是借用库内存的切片，用完调用 `Result.free()`。

## 测试

先在仓库根目录构建库，再跑测试：

```sh
CARGO_TARGET_DIR=target/f cargo build --profile quick -p pagesift-ffi
./bindings/zig/run_tests.sh
```

`run_tests.sh` 用 conda-forge 的 zig（`~/mix-tools/toolchains/zig`，0.17），可用 `ZIG=...` 覆盖。测试 6 项，不联网。

## 用法

```zig
const ps = @import("pagesift.zig");
const r = try ps.convert("<h1>Hi</h1>", null);
defer r.free();
const e = try ps.Engine.init(null);
defer e.deinit();
```

返回 Document JSON 字符串，不解析。

## 库的查找

链接时由 `zig test ... -lc <库文件> -rpath <库目录>` 指定；`run_tests.sh` 用 `PAGESIFT_LIB`（库文件完整路径），默认仓库里的 `target/f/quick/libpagesift.so`。自己的项目里在 `build.zig` 用 `linkSystemLibrary("pagesift")`。

## 安装

`zig fetch --save https://github.com/YOLOW92/pagesift/archive/refs/tags/v<版本>.tar.gz`，再在 `build.zig` 里 `exe.root_module.addImport("pagesift", b.dependency("pagesift", .{}).module("pagesift"))`。`build.zig` 和 `build.zig.zon` 在仓库根，版本来自主仓库的标签 `v<版本>`（不用另外打标签；`build.zig.zon` 里的版本要和标签一致）。包里不带库：先安装 GitHub Release 里的 C 库，构建时加 `--search-prefix <前缀>`（含 `lib/`），运行时库要在动态链接器的路径上。

打包脚本是 `package.sh`（`--ffi-dir` 用 Release 里解开的三个平台的 C 库，或 `--local-lib` 用本机一个库），它会在干净目录里从打好的包安装并跑一次 convert 和 parse。

## 发布时需要的产物

`libpagesift.so`（或 `.dylib`、`.dll`）和 `pagesift.zig`。
