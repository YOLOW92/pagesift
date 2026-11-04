# libpagesift：Java 绑定（FFM）

`src/dev/pagesift/Pagesift.java` 用 JDK 内置的 FFM API（`java.lang.foreign`）调用 C ABI，没有第三方依赖。
需要 JDK 25（JDK 22 起 FFM 是正式版；这里用 `--release 25` 编译）。

## 测试

先在仓库根目录构建库，再跑测试：

```sh
CARGO_TARGET_DIR=target/f cargo build --profile quick -p pagesift-ffi
./bindings/java/run_tests.sh
```

`JAVA_HOME` 没设时用 `~/mix-tools/toolchains/jdk-25`。测试 10 项，不联网，返回非零表示有失败。
测试运行时带 `--enable-native-access=ALL-UNNAMED`，去掉会打印 JDK 的受限方法警告。

## 用法

```java
import dev.pagesift.Pagesift;

String md = Pagesift.convert("<h1>Hi</h1>", null);      // Document JSON 字符串
try (Pagesift.Engine e = Pagesift.Engine.create(null)) {
    String doc = e.parse(csvBytes, "scores.csv", null);
}
```

方法返回 Document JSON 字符串，不解析，需要的话用自己的 JSON 库。引擎创建失败抛 `IllegalArgumentException`。
运行时加 `--enable-native-access=ALL-UNNAMED`，并用 `-Dpagesift.lib=/path/to/libpagesift.so` 指定库；
不指定时先用 jar 里自带的库，再用系统库名（见下面“库的查找”）。

## 库的查找

加载顺序：系统属性 `pagesift.lib` 或环境变量 `PAGESIFT_LIB`（库文件完整路径）；然后 jar 里自带的库；最后是系统库名 `libpagesift.so` / `libpagesift.dylib`。不会联网下载。

## 安装

包发布后（Maven Central，坐标 `io.github.yolow92:pagesift`，Java 包名仍是 `dev.pagesift`，JDK 22+）：

```xml
<dependency>
  <groupId>io.github.yolow92</groupId>
  <artifactId>pagesift</artifactId>
  <version>1.1.0</version>
</dependency>
```

jar 里带三个平台的库（`META-INF/native/linux-x86_64`、`linux-aarch64`、`macos-aarch64`），运行时解压到临时文件加载。只有 Linux x86_64、Linux aarch64、macOS aarch64 三个平台，没有 Windows 版本。

打包脚本是 `package.sh`（`--ffi-dir` 用 Release 里解开的三个平台的 C 库，或 `--local-lib` 用本机一个库），它会在干净目录里从打好的包安装并跑一次 convert 和 parse。

## 发布时需要的产物

`libpagesift.so`（或 `.dylib`、`.dll`）和 `pagesift.h`；Java 那边是 `Pagesift.java` 编译出的 class 文件。
