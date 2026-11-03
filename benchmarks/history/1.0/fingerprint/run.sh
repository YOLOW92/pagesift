#!/usr/bin/env bash
# 录本机 Chrome 的 ClientHello 和 HTTP/2、HTTP/1.1 特征，再录 pagesift 开指纹模拟时的同样内容，逐项比较。
#
#   benchmarks/history/1.0/fingerprint/run.sh [pagesift 二进制]
#
# 全程只连本机。需要 google-chrome、openssl（3.5 起才有 X25519MLKEM768）、python3。
# 结果写进 results/<日期>/：chrome-*.jsonl、pagesift-*.jsonl、compare.txt。
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
PAGESIFT="${1:-$HERE/../../../../target/release/pagesift}"
CHROME="${CHROME:-google-chrome}"
PORT="${PORT:-18443}"
OUT="$HERE/results/$(date +%F)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"; kill $(jobs -p) 2>/dev/null' EXIT
mkdir -p "$OUT"
rm -f "$OUT"/*.jsonl

# 自建 CA：Chrome 用 --ignore-certificate-errors，pagesift 用 SSL_CERT_FILE 信任它（校验没关）
bash "$HERE/mkcerts.sh" "$WORK" ec

serve() { # <alpn> <输出文件>
    python3 -I "$HERE/capture.py" --cert "$WORK/cert.pem" --key "$WORK/key.pem" --port "$PORT" --alpn "$1" --out "$2" >/dev/null 2>&1 &
    SERVER=$!
    for _ in $(seq 50); do
        python3 -I -c "import socket;socket.create_connection(('127.0.0.1',$PORT),1).close()" 2>/dev/null && return
        sleep 0.1
    done
}

chrome() { # <输出文件>
    LANGUAGE=en_US.UTF-8 LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8 timeout 30 "$CHROME" --headless=new --user-data-dir="$WORK/profile-$RANDOM" \
        --ignore-certificate-errors --lang=en-US --no-first-run --disable-gpu --disable-extensions \
        --disable-background-networking --disable-component-update --virtual-time-budget=2000 \
        --dump-dom "https://localhost:$PORT/" >/dev/null 2>&1
}

pagesift() { # 开模拟抓一次
    SSL_CERT_FILE="$WORK/ca.pem" PAGESIFT_CONFIG="$WORK/pagesift.toml" timeout 30 "$PAGESIFT" scrape --engine http --format json \
        "https://localhost:$PORT/" >"$1" 2>&1
}

cat >"$WORK/pagesift.toml" <<EOF
[fetch]
browser_fingerprint = "chrome"
env_proxy = false
allow_hosts = ["localhost"]
[policy]
robots = false
EOF

"$CHROME" --version | tee "$OUT/chrome-version.txt"
for proto in h2 http/1.1; do
    tag="${proto//\//}"
    serve "$proto" "$OUT/chrome-$tag.jsonl"
    chrome
    kill "$SERVER" 2>/dev/null; wait "$SERVER" 2>/dev/null
    if [ -x "$PAGESIFT" ]; then
        serve "$proto" "$OUT/pagesift-$tag.jsonl"
        pagesift "$OUT/pagesift-$tag.out"
        kill "$SERVER" 2>/dev/null; wait "$SERVER" 2>/dev/null
    fi
done
if [ -x "$PAGESIFT" ]; then
    python3 -I "$HERE/compare.py" "$OUT" | tee "$OUT/compare.txt"
    exit "${PIPESTATUS[0]}"
fi
echo "no pagesift binary at $PAGESIFT; recorded Chrome only" >&2
