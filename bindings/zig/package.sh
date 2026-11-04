#!/usr/bin/env bash
# Zig 包在仓库根的 build.zig / build.zig.zon，用户从主仓库的 v<版本> 标签压缩包 zig fetch（不用另外打标签）。
# 这个脚本在干净目录里实测：复制 .zon 的 .paths 列出的文件当作“解包后的包”，新建一个用 .path 依赖它的项目，
# `zig build`（--search-prefix 指到装好 C 库的前缀），运行时 LD_LIBRARY_PATH，跑 convert 和 parse。不设 PAGESIFT_LIB。
# 用法见 ../pkg-common.sh：package.sh [--version V] (--ffi-dir DIR | --local-lib FILE)
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/../pkg-common.sh"
pkg_parse_args "$@"
pkg_prepare_libs
zig="${ZIG:-$HOME/mix-tools/toolchains/zig/bin/zig}"
zonv="$(sed -n 's/^ *\.version = "\(.*\)",/\1/p' "$PKG_ROOT/build.zig.zon")"
[ "$zonv" = "$(sed -n 's/^version = "\(.*\)"/\1/p' "$PKG_ROOT/Cargo.toml" | head -1)" ] || echo "注意：build.zig.zon 的版本 $zonv 和 Cargo.toml 不一致"
[ "$zonv" = "$PKG_VERSION" ] || echo "注意：build.zig.zon 的版本 $zonv 和 --version $PKG_VERSION 不同，发版前要改"
echo "要打的标签：v$PKG_VERSION（主仓库的发布标签，不用另外打）"
c="$PKG_WORK/consumer"; pk="$c/pkg"; mkdir -p "$pk"
# 按 .paths 复制
for f in $(sed -n '/\.paths/,/}/p' "$PKG_ROOT/build.zig.zon" | grep -o '"[^"]*"' | tr -d '"'); do
  mkdir -p "$pk/$(dirname "$f")"; cp "$PKG_ROOT/$f" "$pk/$f"
done
cd "$c"
cat > build.zig.zon <<ZON
.{
    .name = .consumer,
    .version = "0.0.0",
    .fingerprint = 0x705b3727e7c7e2ba,
    .dependencies = .{ .pagesift = .{ .path = "pkg" } },
    .paths = .{ "build.zig", "build.zig.zon", "main.zig", "pkg" },
}
ZON
cat > build.zig <<'ZIG'
const std = @import("std");
pub fn build(b: *std.Build) void {
    const ps = b.dependency("pagesift", .{});
    const exe = b.addExecutable(.{ .name = "app", .root_module = b.createModule(.{
        .root_source_file = b.path("main.zig"),
        .target = b.standardTargetOptions(.{}),
        .optimize = .ReleaseSafe,
    }) });
    exe.root_module.addImport("pagesift", ps.module("pagesift"));
    b.installArtifact(exe);
}
ZIG
cat > main.zig <<'ZIG'
const std = @import("std");
const ps = @import("pagesift");
pub fn main() !void {
    std.debug.print("version {s}\n", .{ps.version()});
    const r = try ps.convert("<h1>Hi</h1><p>World</p>", null);
    defer r.free();
    std.debug.print("CONVERT {s}\n", .{r.text()});
    const e = try ps.Engine.init(null);
    defer e.deinit();
    const p = try e.parse("name,score\nalice,1\nbob,2\n", "scores.csv", null);
    defer p.free();
    std.debug.print("PARSE {s}\n", .{p.text()});
}
ZIG
prefix="$(dirname "$(dirname "$PKG_HOST_LIB")")"
ZIG_GLOBAL_CACHE_DIR="$PKG_WORK/zg" ZIG_LOCAL_CACHE_DIR="$PKG_WORK/zc" "$zig" build --search-prefix "$prefix" 2>&1 | tail -15
res="$(env -u PAGESIFT_LIB LD_LIBRARY_PATH="$prefix/lib" ./zig-out/bin/app 2>&1)"
echo "$res" | cut -c1-160
pkg_expect "$res" '"success":true' "convert/parse 成功"
pkg_expect "$res" "# Hi" "convert 输出标题"
pkg_expect "$res" "| alice | 1 |" "parse 输出表格行"
echo "package.sh zig 通过"
