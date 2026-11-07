# pagesift for Java

A binding of [pagesift](https://github.com/YOLOW92/pagesift) using the JDK's built-in FFM API (`java.lang.foreign`), with no third-party dependencies. It needs JDK 22 or newer.

## Install

```xml
<dependency>
  <groupId>io.github.yolow92</groupId>
  <artifactId>pagesift</artifactId>
  <version>1.2.0</version>
</dependency>
```

The package includes the library for Linux x86_64, Linux aarch64 and macOS arm64. There is no Windows library.

## Usage

```java
import dev.pagesift.Pagesift;

String md = Pagesift.convert("<h1>Hi</h1>", null);
try (Pagesift.Engine e = Pagesift.Engine.create(null)) {
    String doc = e.parse(csvBytes, "scores.csv", null);
}
```

Functions return the Document as a JSON string and do not parse it. Creating an engine with a bad configuration throws `IllegalArgumentException`. Run with `--enable-native-access=ALL-UNNAMED` to avoid the JDK's restricted-method warning.

## Finding the library

1. The system property `pagesift.lib` or the environment variable `PAGESIFT_LIB` (full path to the library file).
2. The library inside the jar.
3. The system library name (`libpagesift.so`, `libpagesift.dylib`).

## Tests

From the repository root:

```sh
CARGO_TARGET_DIR=target/f cargo build --profile quick -p pagesift-ffi
./bindings/java/run_tests.sh
```

The tests do not use the network. Set `JAVA_HOME` to choose the JDK.

## License

AGPL-3.0-only, with commercial licensing available. See the [main repository](https://github.com/YOLOW92/pagesift).
