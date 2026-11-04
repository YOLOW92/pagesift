# libpagesift：PHP 绑定（ext-ffi）

`src/Pagesift.php` 用 PHP 自带的 FFI 扩展调用 C ABI，没有 Composer 依赖。

## 测试

先在仓库根目录构建库，再跑测试：

```sh
CARGO_TARGET_DIR=target/f cargo build --profile quick -p pagesift-ffi
./bindings/php/run_tests.sh
```

`run_tests.sh` 默认用 `~/mix-tools/toolchains/php-ffi/bin/php`（8.5.12，带 ext-ffi），也可以用 `PHP=...` 指定。
测试 10 项，不联网。

## 用法

```php
require 'bindings/php/src/Pagesift.php';

$doc = \Pagesift\Pagesift::convert('<h1>Hi</h1>');   // Document JSON 字符串
```

方法返回 Document JSON 字符串，不解析。需要 `ffi.enable=1`（CLI 默认开启）。

## 库的查找

加载顺序：环境变量 `PAGESIFT_LIB`（库文件完整路径）；然后系统库名 `libpagesift.so` / `libpagesift.dylib`（按 `LD_LIBRARY_PATH`、ldconfig 查找）。`run_tests.sh` 默认把 `PAGESIFT_LIB` 设为仓库里的 `target/f/quick/libpagesift.so`。

## 安装

包发布后（Packagist，包名 `yolow92/pagesift`，要求 PHP 8.1+ 和 ext-ffi）：`composer require yolow92/pagesift`。包由单独的仓库 `YOLOW92/pagesift-php` 提供（`publish-php.yml` 把本目录同步过去并打同名标签）。包里不带库：先安装 GitHub Release 里的 `pagesift-ffi-<标签>-<目标>.tar.gz`（`lib/` 放进系统库路径），或设置 `PAGESIFT_LIB`。

打包脚本是 `package.sh`（`--ffi-dir` 用 Release 里解开的三个平台的 C 库，或 `--local-lib` 用本机一个库），它会在干净目录里从打好的包安装并跑一次 convert 和 parse。

## 发布时需要的产物

`libpagesift.so`（或 `.dylib`、`.dll`）和 `src/Pagesift.php`。
