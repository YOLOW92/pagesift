#!/usr/bin/env bash
# 用 pandoc 3.9 转换一个集合为 GFM。用法：run_pandoc.sh <集目录> <输出目录>
P=~/.venvs/label/lib/python3.12/site-packages/pypandoc/files/pandoc; mkdir -p "$2"
for f in "$1"/*/*.*; do case "$f" in *.tsv) continue;; esac
  ext=${f##*.}; timeout 300 $P -f $ext -t gfm --wrap=none "$f" -o "$2/$(basename "$f").md" 2>/dev/null || : > "$2/$(basename "$f").md"
done
