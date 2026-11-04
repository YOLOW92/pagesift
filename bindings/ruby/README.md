# libpagesift：Ruby 绑定（fiddle）

`pagesift.rb` 用 Ruby 标准库 `fiddle` 调用 C ABI，没有 gem 依赖。测试用标准库的 minitest。

## 测试

先在仓库根目录构建库，再跑测试：

```sh
CARGO_TARGET_DIR=target/f cargo build --profile quick -p pagesift-ffi
./bindings/ruby/run_tests.sh
```

`run_tests.sh` 默认用 `~/mix-tools/toolchains/ruby/bin/ruby`（4.0.9），也可以用 `RUBY=...` 指定。
测试 8 项，不联网。

## 用法

```ruby
require_relative 'bindings/ruby/pagesift'

md = Pagesift.convert('<h1>Hi</h1>')           # Document JSON 字符串
engine = Pagesift::Engine.create
doc = engine.parse(csv_bytes, 'scores.csv')
engine.close
```

返回 Document JSON 字符串，不解析。`Engine#close` 之后再调用抛 `IOError`；忘记关时由 GC 释放。

## 库的查找

加载顺序：环境变量 `PAGESIFT_LIB`（库文件完整路径）；然后 gem 里自带的库；最后是系统库名。`run_tests.sh` 默认把 `PAGESIFT_LIB` 设为仓库里的 `target/f/quick/libpagesift.so`。

## 安装

包发布后（RubyGems，gem 名 `pagesift`）：`gem install pagesift`，然后 `require 'pagesift'`。gem 里带三个平台的库（`lib/pagesift/native/x86_64-linux`、`aarch64-linux`、`arm64-darwin`）。只有 Linux x86_64、Linux aarch64、macOS aarch64 三个平台，没有 Windows 版本。

打包脚本是 `package.sh`（`--ffi-dir` 用 Release 里解开的三个平台的 C 库，或 `--local-lib` 用本机一个库），它会在干净目录里从打好的包安装并跑一次 convert 和 parse。

## 发布时需要的产物

`libpagesift.so`（或 `.dylib`、`.dll`）和 `pagesift.rb`。
