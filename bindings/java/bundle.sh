#!/usr/bin/env bash
# 把 Maven 布局目录做成 Central Portal 的上传包：签名（可选）、md5/sha1 校验和、zip。
# 用法：bundle.sh <布局目录> <输出zip> [--sign]
#   布局目录是 package.sh 产出的 maven/（里面是 io/github/yolow92/pagesift/<版本>/ 的 jar、pom）。
#   --sign：用当前 GNUPGHOME 里的默认密钥给每个文件生成 .asc；口令从环境变量 MAVEN_GPG_PASSPHRASE 读（可为空）。
# 规则来自 Central 文档：每个文件都要 .asc（--sign 时）、.md5、.sha1；.asc 和校验和文件本身不再签名、不再算校验和。
set -euo pipefail
layout="$1"; out="$2"; sign="${3:-}"
layout="$(cd "$layout" && pwd)"
mkdir -p "$(dirname "$out")"; out="$(cd "$(dirname "$out")" && pwd)/$(basename "$out")"
rm -f "$out"

hex() { # 算法 文件
  case "$1" in
    md5) if command -v md5sum >/dev/null; then md5sum "$2" | cut -d' ' -f1; else openssl dgst -md5 -r "$2" | cut -d' ' -f1; fi ;;
    sha1) if command -v sha1sum >/dev/null; then sha1sum "$2" | cut -d' ' -f1; else openssl dgst -sha1 -r "$2" | cut -d' ' -f1; fi ;;
  esac
}

find "$layout" -type f ! -name '*.asc' ! -name '*.md5' ! -name '*.sha1' ! -name '*.sha256' ! -name '*.sha512' | sort | while read -r f; do
  if [ "$sign" = "--sign" ]; then
    rm -f "$f.asc"
    gpg --batch --yes --pinentry-mode loopback --passphrase-fd 3 --armor --detach-sign -o "$f.asc" "$f" 3<<<"${MAVEN_GPG_PASSPHRASE:-}"
  fi
  hex md5 "$f" > "$f.md5"
  hex sha1 "$f" > "$f.sha1"
done
(cd "$layout" && zip -q -r -X "$out" io)
echo "已生成 $out（$(du -h "$out" | cut -f1)）"
