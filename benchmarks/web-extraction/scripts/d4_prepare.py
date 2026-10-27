#!/usr/bin/env python3
"""D4 数据准备：把每个用例的 original_html / mutated_html 落成文件，并生成清单。"""
import json
from pathlib import Path

ROOT = Path("/home/yolow92/pagesift/benchmarks/data/d4")
OUT = Path("/home/yolow92/pagesift/benchmarks/data/showdown/d4")
OUT.mkdir(parents=True, exist_ok=True)
(OUT / "orig").mkdir(exist_ok=True)
(OUT / "mut").mkdir(exist_ok=True)

for tier in ("tier1_mutations_2000", "tier2_chaos_stream_5000"):
    src = ROOT / f"{tier}.jsonl"
    rows = []
    for line in src.open(encoding="utf-8"):
        if not line.strip():
            continue
        c = json.loads(line)
        cid = c["id"].replace("/", "_")
        op = OUT / "orig" / f"{cid}.html"
        mp = OUT / "mut" / f"{cid}.html"
        if not op.exists():
            op.write_text(c["original_html"], encoding="utf-8")
        if not mp.exists():
            mp.write_text(c["mutated_html"], encoding="utf-8")
        rows.append({"id": cid, "tier": tier, "category": c["category"],
                     "selector": c["selector"], "expected": c["expected"],
                     "orig": str(op.resolve()), "mut": str(mp.resolve())})
    with (OUT / f"{tier.split('_')[0]}_{tier.split('_')[-1]}.jsonl").open("w", encoding="utf-8") as f:
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    print(f"{tier}: {len(rows)}")
