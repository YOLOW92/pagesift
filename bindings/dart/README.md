# pagesift for Dart

A binding of [pagesift](https://github.com/YOLOW92/pagesift) using `dart:ffi`, with no package dependencies. It needs Dart 3.3 or newer.

## Install

```sh
dart pub add pagesift
```

The package includes the library for Linux x86_64, Linux aarch64 and macOS arm64. There is no Windows library.

## Usage

```dart
import 'package:pagesift/pagesift.dart' as pagesift;

final md = pagesift.convert('<h1>Hi</h1>');
final e = pagesift.Engine();
final doc = e.parse(csvBytes, 'scores.csv');
e.close();
```

Functions return the Document as a JSON string and do not parse it.

## Finding the library

1. The environment variable `PAGESIFT_LIB` (full path to the library file).
2. The library inside the package.
3. The system library name (`libpagesift.so`, `libpagesift.dylib`).

## Tests

From the repository root:

```sh
CARGO_TARGET_DIR=target/f cargo build --profile quick -p pagesift-ffi
./bindings/dart/run_tests.sh
```

The tests do not use the network. Set `DART` to choose the SDK.

## License

AGPL-3.0-only, with commercial licensing available. See the [main repository](https://github.com/YOLOW92/pagesift).
