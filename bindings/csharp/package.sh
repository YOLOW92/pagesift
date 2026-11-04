#!/usr/bin/env bash
# 打 NuGet 包（Pagesift.<版本>.nupkg）并在干净目录里实测。用法见 ../pkg-common.sh：
#   package.sh [--version V] (--ffi-dir DIR | --local-lib FILE) [--out DIR]
# 产物：--out 下的 Pagesift.<版本>.nupkg（库在 runtimes/<rid>/native/）。
# 实测：新建控制台项目，只用本地 NuGet 源（包缓存也用新目录）安装这个包，不设 PAGESIFT_LIB，跑 convert 和 parse。
# dotnet 默认用 ~/mix-tools/toolchains/dotnet（DOTNET_ROOT 可覆盖）。
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/../pkg-common.sh"
pkg_parse_args "$@"
pkg_prepare_libs
export DOTNET_ROOT="${DOTNET_ROOT:-$HOME/mix-tools/toolchains/dotnet/lib/dotnet}"
export PATH="$DOTNET_ROOT:$PATH"
export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1 DOTNET_SKIP_FIRST_TIME_EXPERIENCE=1

# 1. 把项目复制到临时目录（不带 bin/obj），库按 rid 放好
w="$PKG_WORK/cs"; mkdir -p "$w/proj" "$w/native"
cp "$here/Pagesift/Pagesift.cs" "$here/Pagesift/Pagesift.csproj" "$w/proj/"
for t in "${PKG_TARGETS_PRESENT[@]}"; do
  case "$t" in
    x86_64-unknown-linux-gnu) rid=linux-x64 ;;
    aarch64-unknown-linux-gnu) rid=linux-arm64 ;;
    aarch64-apple-darwin) rid=osx-arm64 ;;
  esac
  mkdir -p "$w/native/$rid"; cp "$PKG_LIBS/$t/lib/$(pkg_lib_name "$t")" "$w/native/$rid/"
done
pkg_copy_legal "$w/native"; cp "$here/README.md" "$w/native/"

# 2. 打包
dotnet pack "$w/proj/Pagesift.csproj" -c Release -p:Version="$PKG_VERSION" -p:PagesiftNativeDir="$w/native" \
  -o "$PKG_OUT" --nologo -v q
nupkg="$PKG_OUT/Pagesift.$PKG_VERSION.nupkg"
[ -f "$nupkg" ] || { echo "没有生成 $nupkg" >&2; exit 1; }
ls -l "$nupkg"; unzip -l "$nupkg" | grep -E "runtimes|LICENSE|NOTICE|README|lib/" || true

# 3. 实测：干净项目 + 本地源
c="$PKG_WORK/consumer"; mkdir -p "$c"
cat > "$c/nuget.config" <<NUGET
<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources>
    <clear />
    <add key="local" value="$PKG_OUT" />
  </packageSources>
</configuration>
NUGET
cat > "$c/app.csproj" <<PROJ
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <OutputType>Exe</OutputType>
    <TargetFramework>net10.0</TargetFramework>
    <ImplicitUsings>enable</ImplicitUsings>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="Pagesift" Version="$PKG_VERSION" />
  </ItemGroup>
</Project>
PROJ
cat > "$c/Program.cs" <<'CS'
using System.Text;
using PagesiftLib;
Console.WriteLine("version " + Pagesift.Version());
Console.WriteLine("CONVERT " + Pagesift.Convert("<h1>Hi</h1><p>World</p>"));
using var e = Pagesift.Engine.Create();
Console.WriteLine("PARSE " + e.Parse(Encoding.UTF8.GetBytes("name,score\nalice,1\nbob,2\n"), "scores.csv"));
CS
export NUGET_PACKAGES="$PKG_WORK/nuget-cache"   # 新的全局包缓存，保证真从本地源装
echo "== 从 nupkg 安装实测（不设 PAGESIFT_LIB）"
cd "$c"
dotnet run -c Release --nologo -v q > "$c/run.txt" 2>&1 || { cat "$c/run.txt"; exit 1; }
res="$(env -u PAGESIFT_LIB -u LD_LIBRARY_PATH dotnet run -c Release --no-build --nologo)"
# 本地库可能是旧版本编的，所以只提示不失败（CI 里库来自同一个 Release，应当一致）
case "$res" in "version $PKG_VERSION"*) echo "  ok   库版本号等于包版本" ;; *) echo "  注意：库报告的版本和包版本不同：$(echo "$res" | head -1)" ;; esac
pkg_expect "$res" '"success":true' "convert/parse 成功"
pkg_expect "$res" "# Hi" "convert 输出标题"
pkg_expect "$res" "| alice | 1 |" "parse 输出表格行"
ls "$c/bin/Release/net10.0/runtimes"/*/native/ 2>/dev/null | head -3
echo "package.sh csharp 通过：$nupkg"
