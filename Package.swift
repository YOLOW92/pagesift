// swift-tools-version:5.9
// Swift Package Manager：用户用 .package(url: "https://github.com/YOLOW92/pagesift.git", from: "<版本>")，
// 版本来自主仓库的 Git 标签 v<版本>（不用另外打标签）。
// 包里不带 C 库：先安装 GitHub Release 里的 pagesift-ffi 包（libpagesift 放进链接器的库路径，
// 或者构建时加 -Xlinker -L<库目录>，运行时用 LD_LIBRARY_PATH / DYLD_LIBRARY_PATH 或 rpath）。
import PackageDescription

let package = Package(
    name: "Pagesift",
    products: [
        .library(name: "Pagesift", targets: ["Pagesift"]),
    ],
    targets: [
        // C 头文件模块：module.modulemap 指向 crates/pagesift-ffi/include/pagesift.h
        .systemLibrary(name: "CPagesift", path: "bindings/swift/CPagesift"),
        .target(
            name: "Pagesift",
            dependencies: ["CPagesift"],
            path: "bindings/swift",
            exclude: ["CPagesift", "test", "README.md", "run_tests.sh", "package.sh"],
            sources: ["Pagesift.swift"],
            linkerSettings: [.linkedLibrary("pagesift")]
        ),
    ]
)
