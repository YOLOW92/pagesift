#!/usr/bin/env python3
"""render_wmb.sh 的汇总：渲染页数、采用数、平均召回；Q_http 对"召回 ≥ τ"的分箱可靠性（τ = 0.5、0.8、0.9）。"""
import json
import statistics
import sys

out = sys.argv[1]
rows = {t: [json.loads(l) for l in open(f"{out}/{t}.jsonl")] for t in ("old", "new")}
print("| | 0.90.0 | 0.91.0 |\n|---|---:|---:|")
for name, f in [
    ("pages", lambda r: len(r)),
    ("errors", lambda r: sum("error" in x for x in r)),
    ("rendered", lambda r: sum(x.get("rendered", False) for x in r)),
    ("rendered by a learned decision", lambda r: sum(bool(x.get("rendered") and x.get("learned")) for x in r)),
    ("rendered or merged result used", lambda r: sum(x.get("adopted", False) for x in r)),
    ("mean recall (higher is better)", lambda r: round(statistics.mean(x["recall"] for x in r if x.get("recall") is not None), 4)),
]:
    print(f"| {name} | {f(rows['old'])} | {f(rows['new'])} |")
pts = [(x["q_http"], x["recall"]) for x in rows["new"] if x.get("q_http") is not None and x.get("recall") is not None]
print(f"\nQ_http vs absolute labels (pages inside the render gate, n = {len(pts)}; 0.91.0 run):\n")
print("| Q_http bin | pages | mean Q_http | share recall ≥ 0.5 | ≥ 0.8 | ≥ 0.9 |\n|---|---:|---:|---:|---:|---:|")
pts.sort()
k = 4
for b in range(k):
    part = pts[b * len(pts) // k:(b + 1) * len(pts) // k]
    if not part:
        continue
    share = lambda t: round(sum(r >= t for _, r in part) / len(part), 3)
    print(f"| {b + 1} | {len(part)} | {round(statistics.mean(q for q, _ in part), 3)} | {share(0.5)} | {share(0.8)} | {share(0.9)} |")
