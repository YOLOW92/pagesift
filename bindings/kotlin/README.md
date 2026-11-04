# libpagesift：Kotlin 绑定（调用 Java 封装）

没有自己的封装层：Kotlin 直接用 `../java/src/dev/pagesift/Pagesift.java`（JDK 22+ 的 FFM）。

## 测试

先在仓库根目录构建库，再跑测试：

```sh
CARGO_TARGET_DIR=target/f cargo build --profile quick -p pagesift-ffi
./bindings/kotlin/run_tests.sh
```

`run_tests.sh` 用 conda-forge 的 kotlin（`~/mix-tools/toolchains/kotlin`，2.4）和 `~/mix-tools/toolchains/jdk-25`，可用 `KOTLIN_HOME`、`JAVA_HOME` 覆盖。conda 版 `kotlinc` 的启动脚本依赖 `CONDA_PREFIX`，脚本直接调用 `libexec/kotlin/bin/kotlinc` 并设好它。测试 9 项，不联网。

## 用法

```kotlin
import dev.pagesift.Pagesift

val md = Pagesift.convert("<h1>Hi</h1>", null)
Pagesift.Engine.create(null).use { e -> e.parse(csvBytes, "scores.csv", null) }
```

返回 Document JSON 字符串，不解析。

## 库的查找

同 Java：`-Dpagesift.lib=<库文件完整路径>`，或环境变量 `PAGESIFT_LIB`（见 `../java/README.md`）；`run_tests.sh` 默认用仓库里的 `target/f/quick/libpagesift.so`。运行时要 `--enable-native-access=ALL-UNNAMED`。

## 安装

包发布后直接用 Java 的包，坐标 `io.github.yolow92:pagesift`（Gradle：`implementation("io.github.yolow92:pagesift:1.1.0")`），Kotlin 里 `import dev.pagesift.Pagesift`。库的加载顺序同 Java 绑定。只有 Linux x86_64、Linux aarch64、macOS aarch64 三个平台，没有 Windows 版本。

打包脚本是 `package.sh`（`--ffi-dir` 用 Release 里解开的三个平台的 C 库，或 `--local-lib` 用本机一个库），它会在干净目录里从打好的包安装并跑一次 convert 和 parse。

## 发布时需要的产物

`libpagesift.so`（或 `.dylib`、`.dll`）和 Java 封装（jar）。
