# libpagesift：Dart 绑定（dart:ffi）

`pagesift.dart` 只用 `dart:ffi` 与 `dart:convert`，不依赖 `package:ffi`（自己用 libc 的 malloc/free 管 C 字符串），所以不需要 pub get。

## 测试

先在仓库根目录构建库，再跑测试：

```sh
CARGO_TARGET_DIR=target/f cargo build --profile quick -p pagesift-ffi
./bindings/dart/run_tests.sh
```

`run_tests.sh` 用 conda-forge 的 dart-sdk（`~/mix-tools/toolchains/dart-sdk`，3.13），可用 `DART=...` 覆盖。测试 10 项，不联网。

## 用法

```dart
import 'pagesift.dart' as pagesift;

final md = pagesift.convert('<h1>Hi</h1>');
final e = pagesift.Engine();
final doc = e.parse(csvBytes, 'scores.csv');
e.close();
```

返回 Document JSON 字符串，不解析。

## 库的查找

加载顺序：环境变量 `PAGESIFT_LIB`（库文件完整路径）；然后包内 `native/<平台>/` 的库；最后是系统库名。`run_tests.sh` 默认把 `PAGESIFT_LIB` 设为仓库里的 `target/f/quick/libpagesift.so`。

## 安装

包发布后（pub.dev，包名 `pagesift`）：`dart pub add pagesift`，然后 `import 'package:pagesift/pagesift.dart'`。包里带三个平台的库（`native/linux-x64`、`linux-arm64`、`macos-arm64`）。第一次发布要手动 `dart pub publish`，之后由 `dart-v<版本>` 标签触发 `publish-dart.yml`。只有 Linux x86_64、Linux aarch64、macOS aarch64 三个平台，没有 Windows 版本。

打包脚本是 `package.sh`（`--ffi-dir` 用 Release 里解开的三个平台的 C 库，或 `--local-lib` 用本机一个库），它会在干净目录里从打好的包安装并跑一次 convert 和 parse。

## 发布时需要的产物

`libpagesift.so`（或 `.dylib`、`.dll`）和 `pagesift.dart`。
