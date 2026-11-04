# libpagesift：C 示例与测试

C 是 C ABI 的参考用法，头文件是 `../../crates/pagesift-ffi/include/pagesift.h`。

## 编译和测试

需要 `cc`（gcc 或 clang）。先在仓库根目录构建库：

```sh
CARGO_TARGET_DIR=target/f cargo build --profile quick -p pagesift-ffi
./bindings/c/run_tests.sh
```

库默认从 `target/f/quick` 找；其他位置用 `PAGESIFT_LIB_DIR=/path/to/lib ./bindings/c/run_tests.sh`。
测试不联网：只跑 HTML 转 Markdown、字节输入的 CSV 解析和错误路径。

## 用法

```c
#include "pagesift.h"

char *doc = pagesift_convert("<h1>Hi</h1>", NULL);   /* Document JSON */
puts(doc);
pagesift_string_free(doc);                           /* 用 pagesift_string_free 释放，不要 free */
```

返回值都是 JSON。成功的结果里 `"success":true`；失败时有 `error_code` 和 `error_message`。
`example.c` 是完整示例，`gcc example.c -I../../crates/pagesift-ffi/include -L<lib> -lpagesift` 即可编译。

## 发布时需要的产物

`libpagesift.so`（Linux）、`libpagesift.dylib`（macOS）、`pagesift.dll`（Windows）和 `pagesift.h`。
只依赖系统的 libc、libm、libgcc_s。
