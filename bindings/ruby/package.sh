#!/usr/bin/env bash
# 打 gem（pagesift-<版本>.gem）并在干净目录里实测。用法见 ../pkg-common.sh：
#   package.sh [--version V] (--ffi-dir DIR | --local-lib FILE) [--out DIR]
# 实测：gem install --local 装到新的 GEM_HOME，再在空目录里 require 'pagesift'，不设 PAGESIFT_LIB，跑 convert 和 parse。
# ruby 默认用 ~/mix-tools/toolchains/ruby（RUBY_HOME 可覆盖）。
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/../pkg-common.sh"
pkg_parse_args "$@"
pkg_prepare_libs
rb="${RUBY_HOME:-$HOME/mix-tools/toolchains/ruby}/bin"
export PATH="$rb:$PATH"

w="$PKG_WORK/gem"; mkdir -p "$w/lib/pagesift/native"
cp "$here/pagesift.rb" "$w/lib/pagesift.rb"
for t in "${PKG_TARGETS_PRESENT[@]}"; do
  case "$t" in
    x86_64-unknown-linux-gnu) d=x86_64-linux ;;
    aarch64-unknown-linux-gnu) d=aarch64-linux ;;
    aarch64-apple-darwin) d=arm64-darwin ;;
  esac
  mkdir -p "$w/lib/pagesift/native/$d"; cp "$PKG_LIBS/$t/lib/$(pkg_lib_name "$t")" "$w/lib/pagesift/native/$d/"
done
pkg_copy_legal "$w"; cp "$here/README.md" "$w/"
sed "s/@VERSION@/$PKG_VERSION/" "$here/pagesift.gemspec.in" > "$w/pagesift.gemspec"
(cd "$w" && gem build pagesift.gemspec --silent -o "$PKG_OUT/pagesift-$PKG_VERSION.gem")
gem="$PKG_OUT/pagesift-$PKG_VERSION.gem"
ls -l "$gem"; tar -xOf "$gem" data.tar.gz | tar -tz | head -20

# 实测：新的 GEM_HOME，空工作目录
# 工具链里的 ruby 把 gem 目录写死了（不认 GEM_HOME），所以用 --install-dir 装到新目录，
# 运行时在脚本开头用 Gem.use_paths 指过去（效果等同用户设了 GEM_HOME）。
gh="$PKG_WORK/gemhome"
sysgems="$(ruby -e "puts Gem.default_dir")"
# fiddle 在这个 Ruby 里是默认 gem，gem install --local 的依赖解析看不到它，所以忽略依赖检查（运行时 require 'fiddle' 是通的）
gem install --local --ignore-dependencies --no-document --install-dir "$gh" "$gem" >/dev/null
mkdir -p "$PKG_WORK/empty" && cd "$PKG_WORK/empty"
echo "== gem install --local 后实测（不设 PAGESIFT_LIB）"
res="$(env -u PAGESIFT_LIB -u LD_LIBRARY_PATH ruby -e '
Gem.use_paths(ARGV[0], [ARGV[0], ARGV[1]])
require "pagesift"
puts "version " + Pagesift.version
puts "CONVERT " + Pagesift.convert("<h1>Hi</h1><p>World</p>")
e = Pagesift::Engine.create
puts "PARSE " + e.parse("name,score\nalice,1\nbob,2\n".b, "scores.csv")
e.close
puts "LIBPATH " + $LOADED_FEATURES.grep(/pagesift/).first.to_s
' "$gh" "$sysgems")"
case "$res" in "version $PKG_VERSION"*) echo "  ok   库版本号等于包版本" ;; *) echo "  注意：库报告的版本和包版本不同：$(echo "$res" | head -1)" ;; esac
pkg_expect "$res" '"success":true' "convert/parse 成功"
pkg_expect "$res" "# Hi" "convert 输出标题"
pkg_expect "$res" "| alice | 1 |" "parse 输出表格行"
pkg_expect "$res" "$gh" "代码来自新装的 gem"
echo "package.sh ruby 通过：$gem"
