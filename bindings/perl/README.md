# libpagesift：Perl 绑定（FFI::Platypus）

`Pagesift.pm` 用 CPAN 的 FFI::Platypus 调用 C ABI；Perl 核心没有 FFI，所以这个模块是必须的。测试用核心的 Test::More。

## 测试

先在仓库根目录构建库，再跑测试：

```sh
CARGO_TARGET_DIR=target/f cargo build --profile quick -p pagesift-ffi
./bindings/perl/run_tests.sh
```

`run_tests.sh` 用系统 perl 5.40，FFI::Platypus 2.12 装在 `~/mix-tools/toolchains/perl5`（`cpanm --notest -L ~/mix-tools/toolchains/perl5 FFI::Platypus`；用 `cpan` 装会改 `~/.bashrc`，别用），可用 `PERL5_LOCAL` 指别处。测试 11 项，不联网。

## 用法

```perl
use Pagesift;
my $md = Pagesift::convert("<h1>Hi</h1>");
my $e = Pagesift::engine();
my $doc = $e->parse($csv_bytes, "scores.csv");
$e->close;
```

返回 Document JSON 字符串，不解析。

## 库的查找

环境变量 `PAGESIFT_LIB` 给出库文件的完整路径；没设时按 `Pagesift.pm` 的位置找仓库里的 `target/f/quick/libpagesift.so`。

## 发布时需要的产物

`libpagesift.so`（或 `.dylib`、`.dll`）和 `Pagesift.pm`；用户要装 FFI::Platypus。
