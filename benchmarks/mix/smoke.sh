#!/usr/bin/env bash
# 冒烟测试：每个工具用 2–4 份现有样本跑一遍 driver，检查装好、协议通、输出非空。不是正式分数。
#   benchmarks/mix/smoke.sh [输出目录，默认 /tmp/mix-smoke] [工具…，默认全部]
# 样本来自本机已有的数据（PAGESIFT_DATA 默认 ~/pagesift/bench/data，OFFICE_DEV 默认修复 3 的 Office 开发集）。
# 各工具同时跑（外层并行），每个工具内部的 worker 数由 driver 按 MemAvailable 和核数定；整体走 bench.lock。
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out=${1:-/tmp/mix-smoke}; shift || true
data=${PAGESIFT_DATA:-$HOME/pagesift/bench/data}
office=${OFFICE_DEV:-$HOME/pagesift/tasks/0.80/fix3/office/dev}
lock=${BENCH_LOCK:-$HOME/pagesift/tasks/0.80/bench.lock}
mkdir -p "$out/manifests"
m=$out/manifests
printf '%s\n' "$data"/snapshots/{1,10,100}.html > "$m/web.txt"
pdfs=$data/olmocr_1403/bench_data/pdfs
printf '%s\n' "$pdfs/tables/008d1dbe3c5a5fd2bac6e4f29acc45e711aa_pg8_pg1.pdf" \
    "$pdfs/multi_column/0005784d0d255f6652180433936fa2998188_page_1_pg1.pdf" \
    "$pdfs/arxiv_math/2502.15977_pg21.pdf" > "$m/pdf.txt"
printf '%s\n' "$office/docx/heading123.docx" "$office/pptx/table_test.pptx" \
    "$office/xlsx/ExcelTables.xlsx" "$office/epub/gutenberg-1342.epub" > "$m/office.txt"

declare -A domain=(
    [crw]=web [trafilatura]=web [readability]=web
    [spider]=web [scrapy]=web [crawl4ai]=web
    [pdf_oxide]=pdf [pymupdf4llm]=pdf [docling]=pdf [docling-gpu]=pdf [docling-auto]=pdf
    [extractous]=office [extractous-py]=office [markitdown]=office [docling-office]=office
)
tools=("$@")
(( ${#tools[@]} )) || tools=(crw trafilatura readability spider scrapy crawl4ai pdf_oxide pymupdf4llm docling docling-gpu extractous extractous-py markitdown docling-office)

run() {
    local t=$1 tool=$1
    [[ $t == docling-office ]] && tool=docling
    python3 -I "$here/driver.py" --tool "$tool" --manifest "$m/${domain[$t]}.txt" --out "$out/$t" --timeout 300 \
        > "$out/$t.summary.json" 2> "$out/$t.driver.log" || echo "{\"tool\":\"$t\",\"driver_failed\":true}" > "$out/$t.summary.json"
}
export -f run
# 吃内存的 docling 系列（每个 worker 3–4 GB）在轻量工具之后逐个跑，免得同时启动时都按同一个 MemAvailable 估算而超卖
light=() heavy=()
for t in "${tools[@]}"; do [[ $t == docling* ]] && heavy+=("$t") || light+=("$t"); done
flock "$lock" bash -c "$(declare -p domain here out m); $(declare -f run); for t in ${light[*]}; do run \$t & done; wait; for t in ${heavy[*]}; do run \$t; done"
for t in "${tools[@]}"; do echo "$t: $(cat "$out/$t.summary.json")"; done
