#!/usr/bin/env bash
# 正式质量轮：在冻结的样本上跑竞品工具，产出原始输出和每份的状态（不测速；测速轮要整机安静，另跑）。
#   benchmarks/mix/run_quality.sh <子类> <工具>...        例：benchmarks/mix/run_quality.sh b1 pdf_oxide pymupdf4llm docling
# 每个“工具 × 子类”是一个作业，各自加一次 bench.lock、跑完就放，别的会话的评测可以插进来；作业内部的 worker 数
# 由 driver.py 按 MemAvailable 和核数定。用模型的工具（docling*）只跑 samples/<子类>.tsv 里 model_subset = 1 的份。
# 单份上限取 limits.toml。输出在 $MIX/runs/quality/<子类>/<工具>/（results.jsonl、summary.json、每份的 .md）。
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
mix=${MIX_TOOLS:-$HOME/mix-tools}
lock=${BENCH_LOCK:-$HOME/pagesift/tasks/0.80/bench.lock}
sub=$1; shift
timeout=$(python3 -I -c "import tomllib,sys;print(tomllib.load(open(sys.argv[1],'rb'))['per_item_s'][sys.argv[2]])" "$here/limits.toml" "$sub")
mem=$(python3 -I -c "import tomllib,sys;print(tomllib.load(open(sys.argv[1],'rb'))['worker_mem_mb'])" "$here/limits.toml")
out=$mix/runs/quality/$sub
mkdir -p "$out/manifests"
for tool in "$@"; do
    man=$out/manifests/$tool.txt
    if [[ $tool == docling* ]]; then
        python3 -I - "$here/samples/$sub.tsv" "$mix/inputs/$sub.manifest" > "$man" <<'EOF'
import sys
cols = None
keep = set()
for line in open(sys.argv[1], encoding="utf-8"):
    parts = line.rstrip("\n").split("\t")
    if cols is None:
        cols = parts
        continue
    r = dict(zip(cols, parts))
    if r.get("model_subset") == "1":
        keep.add(r["id"])
for line in open(sys.argv[2], encoding="utf-8"):
    if line.split("\t", 1)[0] in keep:
        sys.stdout.write(line)
EOF
    else
        cp "$mix/inputs/$sub.manifest" "$man"
    fi
    echo "$(date +%T) 排队 $sub/$tool：$(wc -l < "$man") 份，单份上限 ${timeout}s"
    flock "$lock" python3 -I "$here/driver.py" --tool "$tool" --manifest "$man" --out "$out" \
        --timeout "$timeout" --mem-max "$mem" > "$out/$tool.summary.json" 2> "$out/$tool.driver.log" \
        || echo "{\"tool\":\"$tool\",\"driver_failed\":true}" > "$out/$tool.summary.json"
    echo "$(date +%T) 完成 $sub/$tool：$(cut -c1-200 "$out/$tool.summary.json")"
done
