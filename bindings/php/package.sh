#!/usr/bin/env bash
# 整理出 PHP 拆分仓库（YOLOW92/pagesift-php）的内容并实测。用法见 ../pkg-common.sh：
#   package.sh [--version V] (--ffi-dir DIR | --local-lib FILE) [--out DIR]
# PHP 包不带库，所以库只用来实测：模拟“用户装了 Release 里的 C 库”，通过 LD_LIBRARY_PATH 让系统库名能找到，
# 不设 PAGESIFT_LIB。产物：--out/pagesift-php/（= bindings/php 的文件 + LICENSE + NOTICE，publish-php.yml 把它同步到拆分仓库）和 .tar.gz。
# composer 不在工具链里时，只做 composer.json 的结构检查（json 合法、必填字段、没有 version 字段）。
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/../pkg-common.sh"
pkg_parse_args "$@"
pkg_prepare_libs
php="${PHP:-$HOME/mix-tools/toolchains/php-ffi/bin/php}"

s="$PKG_OUT/pagesift-php"; rm -rf "$s"; mkdir -p "$s"
(cd "$here" && tar --exclude=package.sh -cf - .) | (cd "$s" && tar xf -)
pkg_copy_legal "$s"
(cd "$PKG_OUT" && tar czf "pagesift-php-$PKG_VERSION.tar.gz" pagesift-php)
ls -l "$PKG_OUT/pagesift-php-$PKG_VERSION.tar.gz"; (cd "$s" && find . -type f | sort)

echo "== 语法与元数据检查"
for f in "$s"/src/*.php; do "$php" -l "$f"; done
"$php" -r '
$j = json_decode(file_get_contents($argv[1]), true, 512, JSON_THROW_ON_ERROR);
foreach (["name","description","license","require","autoload"] as $k) { if (empty($j[$k])) { fwrite(STDERR, "composer.json 缺 $k\n"); exit(1); } }
if ($j["name"] !== "yolow92/pagesift") { fwrite(STDERR, "包名不对\n"); exit(1); }
if (isset($j["version"])) { fwrite(STDERR, "composer.json 不应写 version（Packagist 取 Git 标签）\n"); exit(1); }
foreach ($j["autoload"]["files"] as $f) { if (!is_file(dirname($argv[1]) . "/$f")) { fwrite(STDERR, "autoload 文件不存在 $f\n"); exit(1); } }
echo "  ok   composer.json 结构\n";
' "$s/composer.json"
if command -v composer >/dev/null; then (cd "$s" && composer validate --strict); else echo "  （本机没有 composer，未运行 composer validate）"; fi

echo "== 安装实测：C 库在库路径上，不设 PAGESIFT_LIB"
c="$PKG_WORK/consumer"; mkdir -p "$c"
cat > "$c/main.php" <<PHP
<?php
require '$s/src/Pagesift.php';
use Pagesift\\Engine;
use Pagesift\\Pagesift;
echo "version " . Pagesift::version() . "\n";
echo "CONVERT " . Pagesift::convert('<h1>Hi</h1><p>World</p>') . "\n";
\$e = Engine::create();
echo "PARSE " . \$e->parse("name,score\nalice,1\nbob,2\n", 'scores.csv') . "\n";
PHP
res="$(cd "$c" && env -u PAGESIFT_LIB LD_LIBRARY_PATH="$(dirname "$PKG_HOST_LIB")" "$php" -d ffi.enable=1 main.php)"
case "$res" in "version $PKG_VERSION"*) echo "  ok   库版本号等于包版本" ;; *) echo "  注意：库报告的版本和包版本不同：$(echo "$res" | head -1)" ;; esac
pkg_expect "$res" '"success":true' "convert/parse 成功"
pkg_expect "$res" "# Hi" "convert 输出标题"
pkg_expect "$res" "| alice | 1 |" "parse 输出表格行"
# 拆分目录里自带的测试也跑一遍
(cd "$s" && PAGESIFT_LIB="$PKG_HOST_LIB" "$php" -d ffi.enable=1 test/test.php | tail -1)
echo "package.sh php 通过：$s"
