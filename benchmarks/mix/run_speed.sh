#!/usr/bin/env bash
# 测速轮：每个“工具 × 子类 × 路数”是一个作业，单独拿 bench.lock、跑完就放；作业之间串行，测速期间我这边不并行别的作业。
#   MIX_PAGESIFT_BIN=<发布的二进制> benchmarks/mix/run_speed.sh
# 输出 $MIX/runs/speed/w<路数>/<子类>/<工具>/（results.jsonl 每份的 ms、wall_ms、peak_mb；summary.json 的 wall_s）。
# 选题（RULES 第二节 C2 和第四之三节）：
#   网页正文、抓取：A2 全部 269 页，1、4、16 路；
#   PDF：B1 的 10% 模型子集（144 页），所有工具同一批，1 路和 16 路；Docling 另跑 GPU 版（显存限 2 路）；
#   Office：B4 全部 81 份，1 路和 16 路（Docling 只跑它的 10% 子集）。
# 路数是上限：吃内存的工具由 admit.py 按 MemAvailable 压低，实际同时活着的 worker 数记在 summary 的 peak_live_workers。
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
mix=${MIX_TOOLS:-$HOME/mix-tools}
: "${MIX_PAGESIFT_BIN:?要给 MIX_PAGESIFT_BIN}"
run() {  # run <路数> <子集选项> <作业>...
    local w=$1 subset=$2; shift 2
    local specs=()
    for j in "$@"; do specs+=("$j@$w"); done
    MIX_OUT_ROOT=$mix/runs/speed/w$w MIX_SUBSET=$subset python3 -I "$here/run_batch.py" "${specs[@]}"
}
web="a2:crw a2:trafilatura a2:readability a2:pagesift a2:spider a2:scrapy a2:crawl4ai a2:pagesift-crawl"
pdf="b1:pdf_oxide b1:pymupdf4llm b1:docling b1:docling-gpu b1:pagesift-file"
office="b4:extractous b4:markitdown b4:docling b4:pagesift-file"
for w in 1 4 16; do run $w "" $web; done
for w in 1 16; do run $w model $pdf; done
for w in 1 16; do run $w "" $office; done
echo "测速轮全部完成"
