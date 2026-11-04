#!/usr/bin/env bash
# 构建并运行 C# 绑定的测试（.NET 10 SDK，无第三方包）。
# dotnet 在 ~/mix-tools/toolchains/dotnet；库位置用 PAGESIFT_LIB（库文件完整路径），默认仓库里的 target/f/quick。
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
dn_root="${DOTNET_ROOT:-$HOME/mix-tools/toolchains/dotnet/lib/dotnet}"
export DOTNET_ROOT="$dn_root"
export PATH="$dn_root:$PATH"
export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1 DOTNET_SKIP_FIRST_TIME_EXPERIENCE=1
export PAGESIFT_LIB="${PAGESIFT_LIB:-$root/target/f/quick/libpagesift.so}"
cd "$here/Pagesift.Tests"
dotnet run --configuration Release
