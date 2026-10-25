#!/bin/bash
cd ~/pagesift
pkill -f "[e]1_origin.py" 2>/dev/null
sleep 1
mkdir -p bench/results/final/0.70/e1/origin
for spec in "8110 h1" "8111 h2" "8112 h3" "8113 h4"; do
  set -- $spec
  PORT=$1; HOST=$2; DENS=${3:-0.0}
  nohup bench/upstream/.venv/bin/python bench/scripts/e1_origin.py "$PORT" "$HOST" "$DENS" "e1-2026" \
    > "bench/results/final/0.70/e1/origin/$HOST.log" 2>&1 &
  sleep 0.5
done
sleep 2
for p in 8110 8111 8112 8113; do
  printf ":%s index=%s robots=" "$p" "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:$p/)"
  curl -s http://127.0.0.1:$p/robots.txt | head -2 | tr '\n' ' '; echo
done
echo "--- 样例页 ---"
curl -s http://127.0.0.1:8110/a/test1 | head -c 300; echo
echo ORIGIN_UP
