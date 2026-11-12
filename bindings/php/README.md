# pagesift for PHP

A binding of [pagesift](https://github.com/YOLOW92/pagesift) using PHP's FFI extension. It needs PHP 8.1 or newer with `ext-ffi`, and `ffi.enable=1` outside the CLI.

## Install

```sh
composer require yolow92/pagesift
```

The package does not include the library. Download `pagesift-ffi-<tag>-<target>.tar.gz` from [GitHub Releases](https://github.com/YOLOW92/pagesift/releases) (Linux x86_64, Linux aarch64, macOS arm64), or build it with `cargo build --release -p pagesift-ffi`.

## Usage

```php
use Pagesift\Pagesift;

$doc = Pagesift::convert('<h1>Hi</h1>');
```

Functions return the Document as a JSON string and do not parse it.

## Finding the library

1. The environment variable `PAGESIFT_LIB` (full path to the library file).
2. The system library name (`libpagesift.so`, `libpagesift.dylib`).

## Tests

From the repository root:

```sh
CARGO_TARGET_DIR=target/f cargo build --profile quick -p pagesift-ffi
./bindings/php/run_tests.sh
```

The tests do not use the network. Set `PHP` to choose the interpreter.

## License

Apache-2.0. See the [main repository](https://github.com/YOLOW92/pagesift).
