#!/usr/bin/env bash
# 临时 CA 和 localhost 叶子证书：mkcerts.sh <目录> <ec|rsa>，写出 ca.pem、cert.pem、key.pem。
# WebPKI 校验不接受把 CA 证书直接当服务器证书，所以分两张。
set -euo pipefail
dir="$1"
case "$2" in
    ec) key=(-newkey ec -pkeyopt ec_paramgen_curve:P-256) ;;
    rsa) key=(-newkey rsa:2048) ;;
    *) echo "usage: mkcerts.sh <dir> <ec|rsa>" >&2; exit 2 ;;
esac
openssl req -x509 "${key[@]}" -nodes -keyout "$dir/ca-key.pem" -out "$dir/ca.pem" -days 2 \
    -subj "/CN=fingerprint capture CA" -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign 2>/dev/null
openssl req "${key[@]}" -nodes -keyout "$dir/key.pem" -out "$dir/leaf.csr" -subj /CN=localhost 2>/dev/null
printf 'subjectAltName=DNS:localhost\nbasicConstraints=CA:FALSE\nextendedKeyUsage=serverAuth\n' >"$dir/ext"
openssl x509 -req -in "$dir/leaf.csr" -CA "$dir/ca.pem" -CAkey "$dir/ca-key.pem" -CAcreateserial \
    -out "$dir/cert.pem" -days 2 -extfile "$dir/ext" 2>/dev/null
