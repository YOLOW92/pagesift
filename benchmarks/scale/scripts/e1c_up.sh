#!/bin/bash
# E1-C：按固定 seed 注入故障。h1 为对照组（density=0）。
cd ~/pagesift
pkill -f "[e]1_origin.py" 2>/dev/null; sleep 1
OUT=benchmarks/history/0.70/results/e1/origin
mkdir -p "$OUT"
for spec in "8110 h1 0.0" "8111 h2 0.10" "8112 h3 0.0" "8113 h4 0.30"; do
  set -- $spec
  nohup benchmarks/common/vendor/.venv/bin/python benchmarks/scale/scripts/e1_origin.py "$1" "$2" "$3" "e1-2026" \
    > "$OUT/$2.log" 2>&1 &
  sleep 0.5
done
sleep 2
for p in 8110 8111 8112 8113; do
  printf ":%s %s\n" "$p" "$(grep -c . /dev/null; curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:$p/)"
done
echo "--- 故障端点抽样（h4 density=0.30）---"
for k in 429 503 slow truncated malformed; do
  printf "  /fault/%-10s -> %s\n" "$k" "$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 http://127.0.0.1:8113/fault/$k)"
done
printf "  /r/loop     -> %s (follow?)\n" "$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 http://127.0.0.1:8113/r/loop)"
printf "  /go-private -> %s\n" "$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 http://127.0.0.1:8113/go-private)"
echo E1C_UP
