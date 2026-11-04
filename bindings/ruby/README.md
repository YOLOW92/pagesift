# pagesift for Ruby

A binding of [pagesift](https://github.com/YOLOW92/pagesift) using Ruby's standard `fiddle` library. It needs Ruby 3.1 or newer.

## Install

```sh
gem install pagesift
```

The package includes the library for Linux x86_64, Linux aarch64 and macOS arm64. There is no Windows library.

## Usage

```ruby
require 'pagesift'

md = Pagesift.convert('<h1>Hi</h1>')
engine = Pagesift::Engine.create
doc = engine.parse(csv_bytes, 'scores.csv')
engine.close
```

Functions return the Document as a JSON string and do not parse it. Calling an engine after `close` raises `IOError`; an engine you forget to close is freed by the GC.

## Finding the library

1. The environment variable `PAGESIFT_LIB` (full path to the library file).
2. The library inside the package.
3. The system library name (`libpagesift.so`, `libpagesift.dylib`).

## Tests

From the repository root:

```sh
CARGO_TARGET_DIR=target/f cargo build --profile quick -p pagesift-ffi
./bindings/ruby/run_tests.sh
```

The tests do not use the network. Set `RUBY` to choose the interpreter.

## License

AGPL-3.0-only, with commercial licensing available. See the [main repository](https://github.com/YOLOW92/pagesift).
