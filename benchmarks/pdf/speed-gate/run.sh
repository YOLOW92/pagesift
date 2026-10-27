#!/usr/bin/env bash
# 0.80 E：合并后的总体速度与内存闸门（越低越好）。机器空闲后才开始（wait_idle.sh），新旧交替。
#   1. PDF：开发集 13 份 + 留出集 20 份，gate_rounds.py 5 轮（对 0.73 的 lopdf 版本，闸门每份 ≤ +10%）
#   2. HTML 提取：extract_mt 2786 页预载入内存，1 线程、16 线程各 ABBA 6 轮（闸门 ≤ +3%）
#   3. 每请求 CPU：serve /v2/scrape 与 MCP pagesift_scrape，并发 16，ABBA 8 轮（闸门 ≤ +3%）
# 用法：run.sh <新版 bin 目录：pagesift、extract_mt、pdf_one>
set -uo pipefail
cd "$(dirname "$0")/../../.."
NEW=$(realpath "$1"); E=tasks/0.80/e; mkdir -p $E/raw
OLD=tasks/0.73.1/bin/0.73.1/pagesift
OLD_EXTRACT=tasks/0.80/c/bin/base/extract_mt   # 358437c8：HTML 部分与 0.73.1 相同
# 不等空闲：整个脚本放在 bench.lock 里跑

# 1. PDF
P=tasks/0.80/a1/perf
python3 $P/gate_rounds.py $E/raw/pdf.tsv 5 lopdf=tasks/0.80/a0/bin-before/pdf_one a0=tasks/0.80/a0/bin-after/pdf_one a1=$NEW/pdf_one \
  -- tasks/0.80/pdf/*.pdf tasks/0.80/a1/holdout/pdf/*.pdf
python3 $P/summary.py $E/raw/pdf.tsv > $E/raw/pdf-summary.txt
echo "PDF 完成 $(date +%T) $(cat /proc/loadavg)" | tee -a $E/raw/log.txt

# 2. HTML 提取
M=tasks/0.80/c/out/all-manifest.jsonl
: > $E/raw/extract.txt
for r in 1 2 3 4 5 6; do
  if [ $((r % 2)) = 1 ]; then order="old new"; else order="new old"; fi
  for t in 1 16; do for v in $order; do
    b=$OLD_EXTRACT; [ $v = new ] && b=$NEW/extract_mt
    echo "r$r $v t=$t $($b $M $t)" >> $E/raw/extract.txt
  done; done
done
echo "HTML 完成 $(date +%T) $(cat /proc/loadavg)" | tee -a $E/raw/log.txt

# 3. 每请求 CPU（本地站点，合成负载）
python3 -I tasks/0.73.0/step1/site.py tasks/0.73.0/data/wcxb/manifest.jsonl 8370 > $E/raw/site.log 2>&1 &
SITE=$!; sleep 3
S=$(mktemp -d)
printf '[cache]\nenabled = false\n[policy]\ninterval_ms = 0\nper_host_concurrency = 64\n[fetch]\nallow_cidrs = ["127.0.0.0/8"]\n[server]\npersist_jobs = false\n' > $S/c.toml
: > $E/raw/cpu.txt
for r in $(seq 1 8); do
  if [ $((r % 2)) = 1 ]; then order="old new"; else order="new old"; fi
  for kind in serve mcp; do for v in $order; do
    b=$OLD; [ $v = new ] && b=$NEW/pagesift
    bash tasks/0.73.1/cpu_per_req.sh "r$r $v" $b $S/c.toml $kind 16 6 >> $E/raw/cpu.txt
  done; done
done
kill $SITE; wait $SITE 2>/dev/null
echo "CPU 完成 $(date +%T) $(cat /proc/loadavg)" | tee -a $E/raw/log.txt
python3 $E/summary.py > $E/raw/summary.txt
cat $E/raw/summary.txt
