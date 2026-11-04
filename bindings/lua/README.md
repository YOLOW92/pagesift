# libpagesift：LuaJIT 绑定（ffi）

`pagesift.lua` 用 LuaJIT 自带的 `ffi` 调用 C ABI，没有依赖。只支持 LuaJIT，不支持 PUC Lua。

## 测试

先在仓库根目录构建库，再跑测试：

```sh
CARGO_TARGET_DIR=target/f cargo build --profile quick -p pagesift-ffi
./bindings/lua/run_tests.sh
```

`run_tests.sh` 用 conda-forge 的 luajit（`~/mix-tools/toolchains/luajit`，2.1），可用 `LUAJIT=...` 覆盖。测试 10 项，不联网。

## 用法

```lua
local pagesift = require("pagesift")
local md = pagesift.convert("<h1>Hi</h1>")
local e = pagesift.engine()
local doc = e:parse(csv_bytes, "scores.csv")
e:close()
```

返回 Document JSON 字符串，不解析。

## 库的查找

加载顺序：环境变量 `PAGESIFT_LIB`（库文件完整路径）；然后系统库名 `pagesift`（`ffi.load` 补成 `libpagesift.so` / `.dylib`）。`run_tests.sh` 默认把 `PAGESIFT_LIB` 设为仓库里的 `target/f/quick/libpagesift.so`。

## 安装

包发布后（LuaRocks，包名 `pagesift`，只支持 LuaJIT）：`luarocks install pagesift`。rockspec 用 builtin 构建，source 指向主仓库的 Git 标签 `v<版本>`。包里不带库：先安装 GitHub Release 里的 C 库，或设置 `PAGESIFT_LIB`。

打包脚本是 `package.sh`（`--ffi-dir` 用 Release 里解开的三个平台的 C 库，或 `--local-lib` 用本机一个库），它会在干净目录里从打好的包安装并跑一次 convert 和 parse。

## 发布时需要的产物

`libpagesift.so`（或 `.dylib`、`.dll`）和 `pagesift.lua`。
