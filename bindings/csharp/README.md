# libpagesift：C# 绑定（P/Invoke）

`Pagesift/Pagesift.cs` 是类库（`PagesiftLib` 命名空间，类名 `Pagesift`），目标 .NET 10，只用 BCL，没有 NuGet 依赖。

## 测试

先在仓库根目录构建库，再跑测试：

```sh
CARGO_TARGET_DIR=target/f cargo build --profile quick -p pagesift-ffi
./bindings/csharp/run_tests.sh
```

`run_tests.sh` 用 `~/mix-tools/toolchains/dotnet` 里的 SDK（10.0.401）。测试 10 项，不联网。

## 用法

```csharp
using PagesiftLib;

string md = Pagesift.Convert("<h1>Hi</h1>");               // Document JSON 字符串
using var engine = Pagesift.Engine.Create();
string doc = engine.Parse(csvBytes, "scores.csv");
```

方法返回 Document JSON 字符串，不解析。引擎配置出错抛 `ArgumentException`。

## 库的查找

加载顺序：环境变量 `PAGESIFT_LIB`（库文件完整路径）；然后包里自带的 `runtimes/<rid>/native`；最后是系统库名（`LD_LIBRARY_PATH`、应用目录等）。`run_tests.sh` 默认把 `PAGESIFT_LIB` 设为仓库里的 `target/f/quick/libpagesift.so`。

## 安装

包发布后（NuGet，包 id `Pagesift`）：`dotnet add package Pagesift`。包里带三个平台的库（`runtimes/linux-x64/native`、`linux-arm64`、`osx-arm64`）。只有 Linux x86_64、Linux aarch64、macOS aarch64 三个平台，没有 Windows 版本。

打包脚本是 `package.sh`（`--ffi-dir` 用 Release 里解开的三个平台的 C 库，或 `--local-lib` 用本机一个库），它会在干净目录里从打好的包安装并跑一次 convert 和 parse。

## 发布时需要的产物

`libpagesift.so`（或 `libpagesift.dylib`、`pagesift.dll`）。包里的 `Pagesift.dll` 是托管程序集，要和原生库一起发。
