#!/usr/bin/env bash
# 生成真值：pandoc 候选 → docx 样式规则 → xlsx 显示值（LibreOffice en-US）→ 人工修改（edits_<集>.py）。
# 用法：make_truth.sh <dev|holdout>
set -euo pipefail
cd "$(dirname "$0")"; S=$1; PY=~/.venvs/label/bin/python
P=file:///tmp/claude-1000/lo-profile-en; mkdir -p lo-en/$S
for f in $S/xlsx/*.xlsx; do
  [ -s lo-en/$S/$(basename "${f%.xlsx}").html ] || LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8 LANGUAGE=en_US timeout 120 soffice -env:UserInstallation=$P --headless --convert-to html --outdir lo-en/$S "$f" >/dev/null 2>&1
done
$PY -I label.py $S > /dev/null
$PY -I label_docx.py truth/$S $S/docx/*.docx > /dev/null
python3 -I label_xlsx.py lo-en/$S truth/$S > /dev/null
python3 -I label_epub.py truth/$S $S/epub/*.epub > /dev/null
[ -f edits_$S.py ] && $PY -I edits_$S.py truth/$S
echo "真值已生成：truth/$S"
