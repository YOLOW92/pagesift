#!/usr/bin/env bash
# 正文抽取全量对比：Firecrawl 标注集（质量 + 单线程耗时）、AEB 新闻集（token F1）、随机语料（耗时、输出长度）。
# 单线程耗时：各工具同时跑、各占一个物理核（pinned.sh），条件相同；全核吞吐单独跑。所有工具看到同一份冻结的 HTML。
# 用法：bench/fresh/run_extract.sh <结果目录> [prep|run|all]
#   prep：准备输入（浏览器渲染内容太少的页、冻结输入、记 pagesift 全核吞吐），要独占机器
#   run：各工具转换（经 pinned.sh 绑核，可以和其他领域的单线程评测同时跑）和评分
set -uo pipefail
cd "$(dirname "$0")/../.."
R=$1/extract
MODE=${2:-all}
mkdir -p "$R"
T=bench/data/fresh/tools
PY=$T/py/bin/python
C4A=$T/c4ai/bin/python
NODE=$T/node
RT=bench/compare/rust-tools/target/release/pagesift-compare-rust
PS=target/release/pagesift
AEB=bench/data/fresh/data/aeb

cargo build --release -q -p pagesift-content --example extract_jsonl
if [ "$MODE" != run ]; then
# 1. 输入：Firecrawl 集（内容太少的页用浏览器渲染快照）、AEB、随机语料
$PS bench-render bench/data > "$R/render.log" 2>&1
rm -rf bench/data/inputs
# 顺便记下 pagesift 的全核吞吐（pages/s）
$PS bench bench/data --auto --emit-inputs bench/data/inputs > "$R/firecrawl-throughput.txt" 2>&1
python3 -I bench/fresh/aeb.py prepare "$AEB" "$R/aeb" > /dev/null
python3 - "$R/random-manifest.jsonl" <<'PY'
import json, sys
with open(sys.argv[1], "w") as out:
    for line in open("bench/data/random/manifest.jsonl"):
        r = json.loads(line)
        out.write(json.dumps({"id": r["id"], "path": f"bench/data/random/pages/{r['id']}.html",
                              "url": r["final_url"], "base_url": r["final_url"]}) + "\n")
PY
fi
[ "$MODE" = prep ] && exit 0

sets="firecrawl:bench/data/inputs/manifest.jsonl aeb:$R/aeb/manifest.jsonl random:$R/random-manifest.jsonl"
for s in $sets; do
    name=${s%%:*}; m=${s#*:}
    echo "== $name $(date +%T)"
    # 8 个工具同时跑，各占一个物理核（bench/fresh/pinned.sh），条件对所有工具相同
    printf '%s\n' \
        "target/release/examples/extract_jsonl $m $R/$name-pagesift.jsonl" \
        "$PY -I bench/compare/py_adapter.py trafilatura $m $R/$name-trafilatura.jsonl 2>/dev/null" \
        "$PY -I bench/compare/py_adapter.py markitdown $m $R/$name-markitdown.jsonl 2>/dev/null" \
        "node bench/compare/js_adapter.mjs readability $NODE $m $R/$name-readability.jsonl 2>/dev/null" \
        "node bench/compare/js_adapter.mjs defuddle $NODE $m $R/$name-defuddle.jsonl 2>/dev/null" \
        "$RT crw-extract $m $R/$name-crw-extract.jsonl 2>/dev/null" \
        "$RT dom_smoothie $m $R/$name-dom_smoothie.jsonl 2>/dev/null" \
        "CRAWL4AI_NO_CLOUD_NOTICE=1 $C4A -I bench/compare/crawl4ai_adapter.py $m $R/$name-crawl4ai 2>/dev/null" \
        | bench/fresh/pinned.sh
done

# 2. 评分
for f in "$R"/firecrawl-*.jsonl "$R"/firecrawl-crawl4ai/*.jsonl; do
    echo "== $(basename "$f")"
    $PS bench bench/data --auto --external "$f" 2>&1 | tail -12
done > "$R/firecrawl-scores.txt"
python3 -I bench/fresh/aeb.py score "$AEB" "$R/aeb" "$R"/aeb-*.jsonl "$R"/aeb-crawl4ai/*.jsonl > "$R/aeb-scores.txt" 2>&1
python3 - "$R" <<'PY' > "$R/random-timing.txt"
import glob, json, os, statistics, sys
R = sys.argv[1]
for f in sorted(glob.glob(f"{R}/random-*.jsonl") + glob.glob(f"{R}/random-crawl4ai/*.jsonl")):
    if f.endswith("manifest.jsonl"):
        continue
    rows = [json.loads(l) for l in open(f) if l.strip()]
    ms = sorted(r["ms"] for r in rows if r.get("ms") is not None)
    chars = [len(r.get("markdown") or "") for r in rows]
    empty = sum(1 for c in chars if c == 0)
    if ms:
        p = lambda q: ms[min(len(ms) - 1, int(q * len(ms)))]
        print(f"{os.path.basename(f):40} pages {len(rows):5}  p50 {p(.5):8.2f} ms  p95 {p(.95):8.2f} ms  "
              f"mean chars {statistics.mean(chars):8.0f}  empty {empty}")
PY
echo "== extract done $(date +%T)"
