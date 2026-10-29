"""16 路补测用的内存预扣表：每个工具按它 1 路测速的实测峰值 × 1.5（最少 100 MB），对所有工具同一条规则。
    python3 -I benchmarks/mix/v2/peaks.py <子类>... > $MIX/runs-v2/peaks.json
第 1 次 16 路测速按 driver.py 里手写的预估预扣，pagesift 的预估（600 MB）是实测峰值（40 MB）的 15 倍，准入每次放不满就等 1 秒，
吞吐被卡在约“份数 / 1 秒”（B1 三个 pagesift 都是 142.3 份/秒、B4 都是 80 份/秒）。"""
import json
import os
import sys
from pathlib import Path

RUNS = Path(os.environ.get("MIX_SPEED_ROOT", Path(os.environ.get("MIX_TOOLS", Path.home() / "mix-tools")) / "runs-v2/speed")) / "w1"
out = {}
for sub in sys.argv[1:]:
    for d in sorted((RUNS / sub).iterdir()):
        f = d / "results.jsonl"
        if not f.exists():
            continue
        peak = max((json.loads(l).get("peak_mb") or 0 for l in f.read_text().splitlines()), default=0)
        out[d.name] = max(out.get(d.name, 0), max(100, int(peak * 1.5)))
print(json.dumps(out, indent=1, sort_keys=True))
