#!/usr/bin/env bash
# 每个文件单独起一个进程转换，记峰值常驻内存（/usr/bin/time 的 %M，KB，越低越好）。
# 两边都含进程本身的基础内存（Python 解释器和库、Rust 二进制），另测一次空文件作基线。
# 用法：mem.sh <pagesift 二进制> <集目录...>  输出 TSV：文件名、pagesift KB、MarkItDown KB
B=$(realpath "$1"); shift
PY=~/.venvs/markitdown/bin/python
T=$(mktemp -d); printf 'a\n' > $T/empty.csv
# time 的结果写到文件（-o）：被测程序的 stderr 丢掉，不能和 time 的输出混在一起
for f in $T/empty.csv $(for d in "$@"; do ls "$d"/*/*.*; done | grep -v '\.tsv$'); do
  PAGESIFT_CONFIG= /usr/bin/time -o $T/a -f %M "$B" scrape "$f" >/dev/null 2>/dev/null; a=$(tail -1 $T/a)
  /usr/bin/time -o $T/b -f %M $PY -I -W ignore -c "import sys; from markitdown import MarkItDown; MarkItDown().convert(sys.argv[1])" "$f" >/dev/null 2>/dev/null; b=$(tail -1 $T/b)
  echo -e "$(basename "$f")\t$a\t$b"
done
rm -rf $T
