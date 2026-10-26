#!/usr/bin/env python3
"""预热 KaTeX 渲染缓存：把所有数学项里出现的公式先渲染一遍并入库。
之后每个参赛者的评分都直接命中缓存，不再重复启动 Chromium。"""
import json, sys
from pathlib import Path
ROOT = Path("/home/yolow92/pagesift")
sys.path.insert(0, str(ROOT / "benchmarks/pdf/vendor/olmocr"))
from olmocr.bench.katex.render import render_equation  # noqa: E402

BENCH = ROOT / "benchmarks/data/olmocr_1403/bench_data"
seen, ok, fail = set(), 0, 0
for jp in sorted(BENCH.glob("*.jsonl")):
    for line in jp.open(encoding="utf-8"):
        if not line.strip():
            continue
        r = json.loads(line)
        for key in ("math",):
            v = r.get(key)
            if isinstance(v, str) and v.strip() and v not in seen:
                seen.add(v)
                try:
                    render_equation(v); ok += 1
                except Exception:
                    fail += 1
print(json.dumps({"unique_equations": len(seen), "rendered": ok, "failed": fail}))
