#!/usr/bin/env bash
# 运行 Perl 绑定的测试。需要 FFI::Platypus；默认从 ~/mix-tools/toolchains/perl5 找（PERL5_LOCAL 可覆盖）。
# 装法：cpanm --notest -L ~/mix-tools/toolchains/perl5 FFI::Platypus
# 库位置：PAGESIFT_LIB（库文件完整路径），默认仓库里的 target/f/quick/libpagesift.so。
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
export PERL5LIB="${PERL5_LOCAL:-$HOME/mix-tools/toolchains/perl5}/lib/perl5${PERL5LIB:+:$PERL5LIB}"
exec prove -v "$here/test/test.t"
