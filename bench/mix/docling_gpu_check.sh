#!/usr/bin/env bash
# 决定要不要保留 Docling 的 GPU 附表（RULES 第七节第 3 条）：olmOCR 原生数字子集里按固定种子抽 20 页，
# CPU（默认安装）和 GPU（cu126）各跑一次，都是 1 个 worker；第一份含模型懒加载，不计。GPU 快 1.5 倍以上才保留。
#   bench/mix/docling_gpu_check.sh <输出目录>
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out=$1; mkdir -p "$out"
data=${PAGESIFT_DATA:-$HOME/pagesift/bench/data}
lock=${BENCH_LOCK:-$HOME/pagesift/tasks/0.80/bench.lock}
python3 -I - "$data/olmocr_1403/bench_data/pdfs" > "$out/pages.txt" <<'EOF'
import random, sys
from pathlib import Path
root = Path(sys.argv[1])
pages = sorted(str(p) for p in root.glob("*/*.pdf") if not p.parent.name.startswith("old_scans"))
for p in sorted(random.Random(2026102715).sample(pages, 21)):  # 21 = 1 份预热 + 20 份计时
    print(p)
EOF
flock "$lock" bash -c "
python3 -I '$here/driver.py' --tool docling --manifest '$out/pages.txt' --out '$out' --workers 1 --timeout 600 > '$out/cpu.json'
python3 -I '$here/driver.py' --tool docling-gpu --manifest '$out/pages.txt' --out '$out' --workers 1 --timeout 600 > '$out/gpu.json'
"
python3 -I - "$out" <<'EOF'
import json, statistics, sys
from pathlib import Path
out = Path(sys.argv[1])
res = {}
for t in ("docling", "docling-gpu"):
    rows = [json.loads(l) for l in (out / t / "results.jsonl").read_text().splitlines()]
    rows.sort(key=lambda r: r["id"])
    timed = rows[1:]  # id 带清单序号，排序后第一份就是最先跑的那份（模型懒加载），不计
    ok = [r["ms"] for r in timed if r["status"] == "ok"]
    res[t] = {"ok": len(ok), "n": len(timed), "sum_s": round(sum(ok) / 1000, 2), "median_ms": round(statistics.median(ok), 1) if ok else None}
res["speedup_sum"] = round(res["docling"]["sum_s"] / res["docling-gpu"]["sum_s"], 2) if res["docling-gpu"]["sum_s"] else None
res["speedup_median"] = round(res["docling"]["median_ms"] / res["docling-gpu"]["median_ms"], 2) if res["docling-gpu"]["median_ms"] else None
print(json.dumps(res, ensure_ascii=False))
EOF
