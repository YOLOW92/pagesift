#!/usr/bin/env bash
# 用 pagesift CLI 转换一个集合（scrape 本地文件）。用法：run_pagesift.sh <二进制> <集目录> <输出目录>
B=$(realpath "$1"); mkdir -p "$3"; : > "$3/_errors.txt"
for f in "$2"/*/*.*; do case "$f" in *.tsv) continue;; esac
  PAGESIFT_CONFIG= timeout 120 "$B" scrape "$f" > "$3/$(basename "$f").md" 2>>"$3/_errors.txt" || echo "失败 $f" >> "$3/_errors.txt"
done
