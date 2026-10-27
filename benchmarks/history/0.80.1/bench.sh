#!/usr/bin/env bash
# 性能对比：同一组夹具上跑 release 二进制，记录墙钟和 callgrind 指令数（指令数在共享机器上更稳）。
# 用法：bench.sh <夹具目录> <标签>   结果写到本目录 perf-<标签>.tsv；二进制目录可用 PAGESIFT_TARGET 指定
set -euo pipefail
S=$(cd "$(dirname "$0")" && pwd)
R=${PAGESIFT_TARGET:-$S/../../../target/release}
F=${1:?fixtures dir}; label=${2:?label}
out=$S/perf-$label.tsv
: > "$out"

instr() { # 指令总数（callgrind 的 "Collected" 行）
  valgrind --tool=callgrind --callgrind-out-file=/dev/null "$@" 2>&1 >/dev/null | awk '/Collected/ {print $4}'
}

echo "# perf_gate (HTML, median ms, lower is better)" >> "$out"
"$R/examples/perf_gate" 2>&1 | tee -a "$out"

echo "# pdf_one best-of-7 ms, chars" >> "$out"
for i in 1 2 3; do "$R/examples/pdf_one" "$F/paper.pdf" 7; done | tee -a "$out"
echo "# pdf_one instructions (3 runs)" >> "$out"
echo "pdf_one paper.pdf instr $(instr "$R/examples/pdf_one" "$F/paper.pdf" 3)" | tee -a "$out"

echo "# office_bench median ms (runs=7), md bytes" >> "$out"
for f in ascii.csv sjis.csv gbk.csv utf8-cjk.csv; do
  "$R/examples/office_bench" 7 "$F/$f" | tee -a "$out"
done
echo "# office_bench instructions (runs=3)" >> "$out"
for f in ascii.csv sjis.csv gbk.csv utf8-cjk.csv; do
  echo "office_bench $f instr $(instr "$R/examples/office_bench" 3 "$F/$f")" | tee -a "$out"
done

echo "# crawl_gate (pages/s, peak MB; needs *.localhost resolution)" >> "$out"
( "$R/examples/crawl_gate" 2>&1 || echo "crawl_gate failed" ) | tail -3 | tee -a "$out"
