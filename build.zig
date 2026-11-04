// Zig 包：把 bindings/zig/pagesift.zig 暴露成模块 "pagesift"。
// 用户的 build.zig.zon 里 zig fetch --save 本仓库的 v<版本> 标签压缩包（不用另外打标签），
// build.zig 里：const ps = b.dependency("pagesift", .{}); exe.root_module.addImport("pagesift", ps.module("pagesift"));
// 包里不带 C 库：先安装 GitHub Release 里的 pagesift-ffi 包，构建时用 --search-prefix <前缀>（含 lib/）找到它，
// 运行时库要在动态链接器的路径上（LD_LIBRARY_PATH / DYLD_LIBRARY_PATH 或 rpath）。
const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});
    const mod = b.addModule("pagesift", .{
        .root_source_file = b.path("bindings/zig/pagesift.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    mod.linkSystemLibrary("pagesift", .{});
}
