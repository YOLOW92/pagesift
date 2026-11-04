#!/usr/bin/env bash
# 渲染 rockspec（pagesift-<版本>-1.rockspec）并实测。用法：package.sh [--version V] [--out DIR] [--local-lib FILE]
# 本机没有 luarocks（不装），所以不跑 luarocks make：改为（1）luajit 语法校验 rockspec，
# （2）按 rockspec 里 build.modules 的映射把模块放到临时 Lua 路径下，C 库放在库路径上（不设 PAGESIFT_LIB），跑 convert 和 parse。
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/../pkg-common.sh"
pkg_parse_args "$@"
pkg_prepare_libs
lj="${LUAJIT:-$HOME/mix-tools/toolchains/luajit/bin/luajit}"
rs="$PKG_OUT/pagesift-$PKG_VERSION-1.rockspec"
sed "s/@VERSION@/$PKG_VERSION/g" "$here/pagesift.rockspec.in" > "$rs"
echo "== rockspec 语法与字段"
chk="$PKG_WORK/chk.lua"
cat > "$chk" <<'LUA'
local env = {}
local f = assert(loadfile(arg[1], "t", env)); f()
assert(env.package == "pagesift", "package")
assert(env.version == arg[2] .. "-1", "version")
assert(env.source.tag == "v" .. arg[2], "tag")
assert(env.build.type == "builtin", "builtin")
print("modules " .. env.build.modules.pagesift)
LUA
out="$("$lj" "$chk" "$rs" "$PKG_VERSION")"
pkg_expect "$out" "modules bindings/lua/pagesift.lua" "rockspec 可解析，模块映射正确"
echo "== 按 rockspec 模块路径实测（仅 luajit，未用 luarocks）"
t="$PKG_WORK/tree/share/lua/5.1"; mkdir -p "$t"
cp "$PKG_ROOT/bindings/lua/pagesift.lua" "$t/pagesift.lua"
cd "$PKG_WORK"
res="$(env -u PAGESIFT_LIB LD_LIBRARY_PATH="$(dirname "$PKG_HOST_LIB")" LUA_PATH="$t/?.lua;;" "$lj" -e '
local p = require("pagesift")
print("version " .. p.version())
print("CONVERT " .. p.convert("<h1>Hi</h1><p>World</p>"))
local e = p.engine()
print("PARSE " .. e:parse("name,score\nalice,1\nbob,2\n", "scores.csv"))
e:close()')"
pkg_expect "$res" '"success":true' "convert/parse 成功"
pkg_expect "$res" "# Hi" "convert 输出标题"
pkg_expect "$res" "| alice | 1 |" "parse 输出表格行"
echo "package.sh lua 通过（未实测 luarocks）：$rs"
