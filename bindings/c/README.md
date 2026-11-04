# libpagesift: C

The C ABI of [pagesift](https://github.com/YOLOW92/pagesift). The header is `crates/pagesift-ffi/include/pagesift.h`.

## Install

The package does not include the library. Download `pagesift-ffi-<tag>-<target>.tar.gz` from [GitHub Releases](https://github.com/YOLOW92/pagesift/releases) (Linux x86_64, Linux aarch64, macOS arm64), or build it with `cargo build --release -p pagesift-ffi`. The archive holds the library and the header.

C is not published to a package registry.

## Usage

```c
#include "pagesift.h"

char *doc = pagesift_convert("<h1>Hi</h1>", NULL);   /* Document JSON */
puts(doc);
pagesift_string_free(doc);   /* free with pagesift_string_free, not free */
```

Every result is JSON. On success it has `"success":true`; on failure it has `error_code` and `error_message`. `example.c` is a full example:

```sh
gcc example.c -Icrates/pagesift-ffi/include -L<lib dir> -lpagesift
```

The library depends only on the system libc, libm and libgcc_s.

## Tests

From the repository root:

```sh
CARGO_TARGET_DIR=target/f cargo build --profile quick -p pagesift-ffi
./bindings/c/run_tests.sh
```

The tests do not use the network. Set `PAGESIFT_LIB_DIR` to use a library elsewhere.

## License

AGPL-3.0-only, with commercial licensing available. See the [main repository](https://github.com/YOLOW92/pagesift).
