#!/usr/bin/env bash
# 锁内（bench.lock）：最终二进制的 office 耗时与峰值内存，和 MarkItDown 同一时段交替测。不调用 wait_idle。
set -uo pipefail
cd /home/yolow92/pagesift/tasks/0.80/fix3/office
files=$(ls dev/*/*.* holdout/*/*.* | grep -v '\.tsv$')
../bin/office_bench-final 5 $files > raw-time-pagesift-final.tsv
~/.venvs/markitdown/bin/python -I time_markitdown.py 3 $files > raw-time-markitdown-final.tsv
bash mem.sh ../bin/pagesift-final dev holdout > raw-mem-final.tsv
echo 完成 $(date +%T)
