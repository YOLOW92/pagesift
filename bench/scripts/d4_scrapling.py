#!/usr/bin/env python3
"""D4 的 Scrapling 对照：Adaptor.relocate 重定位。"""
import json, statistics, sys, time
from pathlib import Path
try:
    from scrapling.parser import Adaptor
except ImportError:
    print(json.dumps({"error": "scrapling not installed"})); sys.exit(0)

case_file, out_path = sys.argv[1], sys.argv[2]
cases = [json.loads(l) for l in open(case_file, encoding="utf-8") if l.strip()]
rows = []
for c in cases:
    t0 = time.perf_counter()
    try:
        orig = Adaptor(Path(c["orig"]).read_text(encoding="utf-8"))
        target = orig.css(c["selector"])[0]
        mut = Adaptor(Path(c["mut"]).read_text(encoding="utf-8"))
        rel = mut.relocate(target)
        us = (time.perf_counter() - t0) * 1e6
        text = (rel[0].text or "").strip() if rel else ""
        exp = (c["expected"] or "").strip()
        ok = bool(text) and (exp in text or text in exp)
        rows.append({"id": c["id"], "category": c["category"], "found": bool(rel),
                     "ok": ok, "status": "found" if rel else "none", "us": us})
    except Exception as e:
        rows.append({"id": c["id"], "category": c["category"], "found": False,
                     "ok": False, "status": f"error:{type(e).__name__}", "us": 0.0})

by = {}
for r in rows:
    by.setdefault(r["category"], []).append(r)
summary = {"n": len(rows),
           "recall": statistics.fmean(1.0 if r["ok"] else 0.0 for r in rows),
           "found_rate": statistics.fmean(1.0 if r["found"] else 0.0 for r in rows),
           "latency_us": {"mean": round(statistics.fmean(r["us"] for r in rows), 1)},
           "by_category": {k: {"n": len(v), "recall": statistics.fmean(1.0 if x["ok"] else 0.0 for x in v)}
                           for k, v in sorted(by.items())}}
Path(out_path).parent.mkdir(parents=True, exist_ok=True)
Path(out_path).write_text(json.dumps(summary, ensure_ascii=False, indent=2), encoding="utf-8")
print(json.dumps({k: summary[k] for k in ("n", "recall", "found_rate", "latency_us")}, ensure_ascii=False))
