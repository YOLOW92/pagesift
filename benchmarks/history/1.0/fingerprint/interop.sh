#!/usr/bin/env bash
# pagesift 开指纹模拟时和 OpenSSL 的互通：只开 ML-KEM 混合组、HelloRetryRequest、TLS 1.2、服务器选只声明的 CBC 套件（退回普通连接）、
# 自签证书被拒。全程只连本机。需要 openssl 3.5 以上。
#
#   benchmarks/history/1.0/fingerprint/interop.sh <pagesift 二进制>
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
PAGESIFT="$1"
PORT="${PORT:-18444}"
OUT="$HERE/results/$(date +%F)/interop.txt"
WORK="$(mktemp -d)"
SERVER=""
trap 'rm -rf "$WORK"; [ -n "$SERVER" ] && kill "$SERVER" 2>/dev/null' EXIT
mkdir -p "$(dirname "$OUT")"
: >"$OUT"
FAILED=0

bash "$HERE/mkcerts.sh" "$WORK" rsa
mkdir "$WORK/other"
bash "$HERE/mkcerts.sh" "$WORK/other" rsa
cat >"$WORK/pagesift.toml" <<EOF
[fetch]
browser_fingerprint = "chrome"
env_proxy = false
allow_hosts = ["localhost"]
[policy]
robots = false
EOF

fetch() { # 输出 JSON；SSL_CERT_FILE 指向 $1
    SSL_CERT_FILE="$1" PAGESIFT_CONFIG="$WORK/pagesift.toml" timeout 30 "$PAGESIFT" scrape --engine http --format json "https://localhost:$PORT/" 2>&1
}

start() {
    openssl s_server -quiet -accept "$PORT" -cert "$WORK/cert.pem" -key "$WORK/key.pem" -www "$@" >"$WORK/server.log" 2>&1 &
    SERVER=$!
    sleep 0.5
}

stop() {
    kill "$SERVER" 2>/dev/null
    wait "$SERVER" 2>/dev/null
    SERVER=""
}

check() { # <名字> <JSON 里必须有的文本> <证书文件> [必须没有的文本]
    local name="$1" want="$2" trust="$3" absent="${4:-}" got ok=yes
    got="$(fetch "$trust")"
    case "$got" in *"$want"*) ;; *) ok=no ;; esac
    if [ -n "$absent" ]; then
        case "$got" in *"$absent"*) ok=no ;; esac
    fi
    if [ "$ok" = yes ]; then
        echo "ok    $name" | tee -a "$OUT"
    else
        echo "FAIL  $name" | tee -a "$OUT"
        echo "$got" | cut -c1-600 | tee -a "$OUT"
        FAILED=1
    fi
}

start -tls1_3 -groups X25519MLKEM768
check "TLS 1.3, server only accepts X25519MLKEM768 (ML-KEM really used)" '"browserFingerprint": "chrome_156"' "$WORK/ca.pem"
stop

start -tls1_3 -groups X25519
check "TLS 1.3, server only accepts X25519" '"browserFingerprint": "chrome_156"' "$WORK/ca.pem"
stop

start -tls1_3 -groups P-256
check "TLS 1.3, P-256 only (HelloRetryRequest)" '"browserFingerprint": "chrome_156"' "$WORK/ca.pem"
stop

start -tls1_2 -cipher ECDHE-RSA-AES128-GCM-SHA256
check "TLS 1.2, ECDHE-RSA-AES128-GCM-SHA256" '"browserFingerprint": "chrome_156"' "$WORK/ca.pem"
stop

start -tls1_2 -cipher 'ECDHE-RSA-AES128-SHA:ECDHE-RSA-AES128-GCM-SHA256:@SECLEVEL=0' -serverpref
check "TLS 1.2, server prefers declared-only CBC (falls back, warns)" 'fingerprint_fallback' "$WORK/ca.pem" '"browserFingerprint"'
stop

start -tls1_3
check "certificate from a CA outside the trust store is refused" 'TLS_ERROR' "$WORK/other/ca.pem" '"statusCode":200'
stop

# robots.txt 仍按 pagesift 的产品标识匹配：只禁 pagesift 的站点要被拒，只禁 Mozilla 的站点照常抓
mkdir "$WORK/site"
echo '<html><body><p>robots check</p></body></html>' >"$WORK/site/index.html"
cat >"$WORK/robots.toml" <<EOF
[fetch]
browser_fingerprint = "chrome"
env_proxy = false
allow_hosts = ["localhost"]
EOF
python3 -I -m http.server "$PORT" --bind 127.0.0.1 --directory "$WORK/site" >/dev/null 2>&1 &
SERVER=$!
sleep 0.5
robots() { # <名字> <robots.txt 内容> <必须有的文本>
    printf '%b' "$2" >"$WORK/site/robots.txt"
    local got
    got="$(PAGESIFT_CONFIG="$WORK/robots.toml" timeout 30 "$PAGESIFT" scrape --engine http --format json "http://localhost:$PORT/" 2>&1)"
    case "$got" in
        *"$3"*) echo "ok    $1" | tee -a "$OUT" ;;
        *) echo "FAIL  $1" | tee -a "$OUT"; echo "$got" | cut -c1-400 | tee -a "$OUT"; FAILED=1 ;;
    esac
}
robots "robots.txt that disallows pagesift still applies" 'User-agent: pagesift\nDisallow: /\n' 'ROBOTS_DISALLOWED'
robots "robots.txt that only disallows Mozilla does not apply" 'User-agent: Mozilla\nDisallow: /\n' '"browserFingerprint": "chrome_156"'
stop

exit "$FAILED"
