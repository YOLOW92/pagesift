#!/bin/bash
# E1-D：A/B/C 同场混合，单次固定 10 分钟，四阶段。
#  0-2 min 建稳态          : A 内容风暴 + B 正常爬取（h1/h2 无故障）
#  2-4 min 升内容量与并发  : A 加线程，B 增至 4 主机，C 低密度故障主机入场
#  4-6 min 提故障密度+恢复 : 启高密度故障主机，并 kill 一个爬取后重启（中断恢复）
#  6-8 min 最大可持续混合  : 保持满压
#  8-10 min 停新增并排空   : 不再启动新工作，等待在途完成；超时如实计未完成
set -u
cd ~/pagesift
OUT=bench/results/final/0.70/e1/D
rm -rf "$OUT"; mkdir -p "$OUT"
BIN=bench/p4_2_webmainbench/target/release/pagesift-p4-2-webmainbench
CFG=bench/scripts/e1.toml
CORPUS=bench/results/final/0.70/e1/corpus/manifest.jsonl
PIDS=()

start_crawl () { # port host limit tag
  ./target/release/pagesift --config "$CFG" crawl "http://127.0.0.1:$1/" \
    --out "$OUT/$4" --limit "$3" --depth 8 --entire-domain --sitemap include --engine http \
    > "$OUT/log_$4.txt" 2>&1 &
  PIDS+=($!)
}

T0=$(date +%s)
now () { echo $(( $(date +%s) - T0 )); }

# 监控：每分钟采样
bench/upstream/.venv/bin/python bench/scripts/e1d_monitor.py "$OUT" "$T0" 600 &
MON=$!

echo "[$(now)s] phase1: A(32t) + B(h1,h2)"
$BIN "$CORPUS" "$OUT/a1.jsonl" 32 --hard-timeout-secs 600 --soft-warn-secs 300 > "$OUT/log_a1.txt" 2>&1 &
PIDS+=($!)
start_crawl 8110 h1 400 b_h1
start_crawl 8111 h2 400 b_h2

while [ $(now) -lt 120 ]; do sleep 5; done

echo "[$(now)s] phase2: A(96t) + B(h1..h3) + C(h4 低故障)"
$BIN "$CORPUS" "$OUT/a2.jsonl" 96 --hard-timeout-secs 600 --soft-warn-secs 300 > "$OUT/log_a2.txt" 2>&1 &
PIDS+=($!)
start_crawl 8112 h3 400 b_h3
start_crawl 8113 h4 400 c_h4

while [ $(now) -lt 240 ]; do sleep 5; done

echo "[$(now)s] phase3: A(160t) + 故障密度提升 + 中断恢复"
$BIN "$CORPUS" "$OUT/a3.jsonl" 160 --hard-timeout-secs 600 --soft-warn-secs 300 > "$OUT/log_a3.txt" 2>&1 &
PIDS+=($!)
# 中断：杀掉 h2 的爬取后立刻重启（验证能否继续产出、不重复计数）
pkill -f "crawl http://127.0.0.1:8111/" 2>/dev/null
sleep 2
start_crawl 8111 h2r 400 b_h2_resumed

while [ $(now) -lt 360 ]; do sleep 5; done

echo "[$(now)s] phase4: 停新增，保持满压至 8 min"
while [ $(now) -lt 480 ]; do sleep 5; done

echo "[$(now)s] phase5: 排空（不再启动新工作）"
DRAIN_START=$(now)
for p in "${PIDS[@]}"; do kill -0 "$p" 2>/dev/null && echo "  still running pid=$p"; done

# 等到 600 s；仍未结束的如实记未完成
while [ $(now) -lt 600 ]; do
  alive=0
  for p in "${PIDS[@]}"; do kill -0 "$p" 2>/dev/null && alive=$((alive+1)); done
  [ "$alive" -eq 0 ] && break
  sleep 5
done
END=$(now)
alive=0
for p in "${PIDS[@]}"; do kill -0 "$p" 2>/dev/null && alive=$((alive+1)); done
echo "[${END}s] drain done; unfinished_jobs=$alive"
kill $MON 2>/dev/null
echo "unfinished_jobs=$alive" > "$OUT/drain.json"
echo "wall_s=$END" >> "$OUT/drain.json"
echo E1D_DONE
