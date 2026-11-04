# pagesift for Zig

A Zig binding of [pagesift](https://github.com/YOLOW92/pagesift) that declares the C ABI directly, with nothing beyond `std`. Results borrow library memory; call `Result.free()` when done.

## Install

```sh
zig fetch --save https://github.com/YOLOW92/pagesift/archive/refs/tags/v1.1.2.tar.gz
```

Then in `build.zig`:

```zig
exe.root_module.addImport("pagesift", b.dependency("pagesift", .{}).module("pagesift"));
```

The package does not include the library. Download `pagesift-ffi-<tag>-<target>.tar.gz` from [GitHub Releases](https://github.com/YOLOW92/pagesift/releases) (Linux x86_64, Linux aarch64, macOS arm64), or build it with `cargo build --release -p pagesift-ffi`. Build with `--search-prefix <prefix>` (the directory holding `lib/`). At run time the library must be on the dynamic linker's path.

## Usage

```zig
const ps = @import("pagesift");
const r = try ps.convert("<h1>Hi</h1>", null);
defer r.free();
const e = try ps.Engine.init(null);
defer e.deinit();
```

Functions return the Document as a JSON string and do not parse it.

## Tests

From the repository root:

```sh
CARGO_TARGET_DIR=target/f cargo build --profile quick -p pagesift-ffi
./bindings/zig/run_tests.sh
```

The tests do not use the network. Set `ZIG` to choose the compiler.

## License

AGPL-3.0-only, with commercial licensing available. See the [main repository](https://github.com/YOLOW92/pagesift).
