# pagesift for LuaJIT

A binding of [pagesift](https://github.com/YOLOW92/pagesift) using LuaJIT's `ffi`. It works with LuaJIT only, not PUC Lua.

## Install

```sh
luarocks --lua-version 5.1 install pagesift
```

The package does not include the library. Download `pagesift-ffi-<tag>-<target>.tar.gz` from [GitHub Releases](https://github.com/YOLOW92/pagesift/releases) (Linux x86_64, Linux aarch64, macOS arm64), or build it with `cargo build --release -p pagesift-ffi`.

## Usage

```lua
local pagesift = require("pagesift")
local md = pagesift.convert("<h1>Hi</h1>")
local e = pagesift.engine()
local doc = e:parse(csv_bytes, "scores.csv")
e:close()
```

Functions return the Document as a JSON string and do not parse it.

## Finding the library

1. The environment variable `PAGESIFT_LIB` (full path to the library file).
2. The system library name (`libpagesift.so`, `libpagesift.dylib`).

## Tests

From the repository root:

```sh
CARGO_TARGET_DIR=target/f cargo build --profile quick -p pagesift-ffi
./bindings/lua/run_tests.sh
```

The tests do not use the network. Set `LUAJIT` to choose the interpreter.

## License

AGPL-3.0-only, with commercial licensing available. See the [main repository](https://github.com/YOLOW92/pagesift).
