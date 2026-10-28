#!/usr/bin/env bash
# 第 2 版阶段一测速轮（RULES.md 第三节）：每个“工具 × 子类 × 路数”一个作业，单独拿 bench.lock、跑完就放，作业之间串行。
#   MIX_PAGESIFT_BIN=<0.80.9 二进制> benchmarks/mix/v2/run_speed_phase1.sh
# 选题同第 1 版 run_speed.sh：网页正文、抓取用 A2 全部 269 页，1 路和 16 路；PDF 用 B1 的 10% 模型子集（144 页），
# 1 路和 16 路；Office 用 B4 全部 81 份，1 路和 16 路。输出 $MIX/runs-v2/speed/w<路数>/<子类>/<工具>/。
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
mix=${MIX_TOOLS:-$HOME/mix-tools}
: "${MIX_PAGESIFT_BIN:?要给 MIX_PAGESIFT_BIN}"
run() {  # run <路数> <子集选项> <作业>...
    local w=$1 subset=$2; shift 2
    local specs=()
    for j in "$@"; do specs+=("$j@$w"); done
    MIX_OUT_ROOT=$mix/runs-v2/speed/w$w MIX_SUBSET=$subset python3 -I "$here/run_batch.py" "${specs[@]}"
}
web="a2:pagesift a2:pagesift-080 a2:pagesift-serve a2:crw a2:trafilatura a2:readability a2:resiliparse a2:newspaper4k"
crawl="a2:pagesift-crawl a2:pagesift-080-crawl a2:pagesift-serve-crawl a2:spider a2:scrapy a2:crawlee"
pdf="b1:pagesift-file b1:pagesift-080-file b1:pagesift-serve-file b1:pdf_oxide b1:pymupdf4llm b1:kreuzberg b1:mineru-flash"
office="b4:pagesift-file b4:pagesift-080-file b4:pagesift-serve-file b4:extractous b4:markitdown b4:kreuzberg b4:pandoc b4:mineru-flash"
for w in 1 16; do run $w "" $web $crawl; done
for w in 1 16; do run $w model $pdf; done
for w in 1 16; do run $w "" $office; done
echo "阶段一测速轮全部完成"
# 阶段二用模型的工具（同一批 B1 10% 子集 144 页、B4 的 Docling 子集），放在最后：单份最慢
if [[ ${MIX_PHASE2:-0} == 1 ]]; then
    for w in 1 16; do run $w model b1:docling b1:docling-gpu b1:mineru; done
    for w in 1 16; do run $w "" b4:docling; done
    echo "阶段二测速（模型工具）完成"
fi
