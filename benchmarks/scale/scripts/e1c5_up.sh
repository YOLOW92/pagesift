#!/bin/bash
# E1-C 重做：把 Crawl-delay 与故障注入解耦。
#   h1 无delay 无故障（对照） | h2 无delay 10%故障 | h3 全站禁止 | h4 无delay 30%故障
#   h5 有delay(1s) 无故障 —— 单独验证 Crawl-delay（与故障正交）
cd ~/pagesift
pkill -f "[e]1_origin.py" 2>/dev/null; sleep 1
OUT=bench/results/final/0.70/e1/origin
mkdir -p "$OUT"
nohup bench/upstream/.venv/bin/python bench/scripts/e1_origin.py 8110 h1 0.0 e1-2026 0 > "$OUT/h1.log" 2>&1 &
nohup bench/upstream/.venv/bin/python bench/scripts/e1_origin.py 8111 h2 0.10 e1-2026 0 > "$OUT/h2.log" 2>&1 &
nohup bench/upstream/.venv/bin/python bench/scripts/e1_origin.py 8112 h3 0.0 e1-2026 0 > "$OUT/h3.log" 2>&1 &
nohup bench/upstream/.venv/bin/python bench/scripts/e1_origin.py 8113 h4 0.30 e1-2026 0 > "$OUT/h4.log" 2>&1 &
nohup bench/upstream/.venv/bin/python bench/scripts/e1_origin.py 8114 h5 0.0 e1-2026 1 > "$OUT/h5.log" 2>&1 &
sleep 3
for p in 8110 8111 8112 8113 8114; do
  printf ":%s %s | " "$p" "$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 http://127.0.0.1:$p/)"
  curl -s --max-time 5 "http://127.0.0.1:$p/robots.txt" | tr '\n' ' '; echo
done
echo E1C5_UP
