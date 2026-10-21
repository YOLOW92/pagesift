#!/usr/bin/env bash
# HTML → Markdown 保真度对决：按种子生成随机文档，所有工具各转一遍，统一评分。
# 用法：bench/markdown/run.sh [种子...]（默认 1 2 3），每个种子 200 篇；结果在 $OUT（默认 bench/markdown/out）。
set -euo pipefail
cd "$(dirname "$0")/../.."
OUT=${OUT:-bench/markdown/out}
PY=${PY:-bench/data/fresh/tools/py/bin/python}          # markdownify、html2text、markitdown
C4A=${C4A:-bench/data/fresh/tools/c4ai/bin/python}        # crawl4ai，以及评分用的 markdown-it-py
NODE_DIR=${NODE_DIR:-bench/data/fresh/tools/node}       # turndown、defuddle
cargo build --release -q -p pagesift-content --example md_convert
for seed in ${@:-1 2 3}; do
    D=$OUT/seed$seed
    rm -rf "$D"
    "$C4A" -I bench/markdown/fidelity.py gen "$D/html" 200 "$seed"
    # 七个转换器同时跑，各占一个物理核（bench/fresh/pinned.sh），单页耗时条件相同
    printf '%s\n' \
        "target/release/examples/md_convert $D/html $D/pagesift --full > $D/t-pagesift.txt" \
        "$PY -I bench/markdown/py_tools.py $D/html $D markdownify > $D/t-markdownify.txt 2>/dev/null" \
        "$PY -I bench/markdown/py_tools.py $D/html $D html2text > $D/t-html2text.txt 2>/dev/null" \
        "$PY -I bench/markdown/py_tools.py $D/html $D markitdown > $D/t-markitdown.txt 2>/dev/null" \
        "$C4A -I bench/markdown/py_tools.py $D/html $D crawl4ai > $D/t-crawl4ai.txt 2>/dev/null" \
        "node bench/markdown/js_tools.mjs $NODE_DIR $D/html $D turndown > $D/t-turndown.txt 2>/dev/null" \
        "node bench/markdown/js_tools.mjs $NODE_DIR $D/html $D defuddle > $D/t-defuddle.txt 2>/dev/null" \
        | bench/fresh/pinned.sh
    cat "$D"/t-*.txt | tee "$D/timing.txt"
    "$C4A" -I bench/markdown/fidelity.py score "$D/html" pagesift="$D/pagesift" markdownify="$D/markdownify" \
        html2text="$D/html2text" markitdown="$D/markitdown" crawl4ai="$D/crawl4ai" turndown="$D/turndown" \
        defuddle="$D/defuddle" | tee "$D/score.txt"
done
