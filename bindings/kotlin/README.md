# pagesift for Kotlin

Kotlin uses the [Java binding](../java/README.md) of [pagesift](https://github.com/YOLOW92/pagesift) directly. It needs JDK 22 or newer.

## Install

```kotlin
implementation("io.github.yolow92:pagesift:1.2.2")
```

The package includes the library for Linux x86_64, Linux aarch64 and macOS arm64. There is no Windows library.

## Usage

```kotlin
import dev.pagesift.Pagesift

val md = Pagesift.convert("<h1>Hi</h1>", null)
Pagesift.Engine.create(null).use { e -> e.parse(csvBytes, "scores.csv", null) }
```

Functions return the Document as a JSON string and do not parse it. Run with `--enable-native-access=ALL-UNNAMED`.

## Finding the library

The same as [Java](../java/README.md).

## Tests

From the repository root:

```sh
CARGO_TARGET_DIR=target/f cargo build --profile quick -p pagesift-ffi
./bindings/kotlin/run_tests.sh
```

The tests do not use the network. Set `KOTLIN_HOME` and `JAVA_HOME` to choose the tools.

## License

AGPL-3.0-only, with commercial licensing available. See the [main repository](https://github.com/YOLOW92/pagesift).
