"""把 smoke.sh 的输出目录汇成一张 Markdown 表（填 tools.lock.md 用）。
    python3 -I bench/mix/smoke_table.py <冒烟输出目录>…   （多个目录时，同名工具以后面的为准）
"""
import json
import sys
from pathlib import Path

rows = {}
for d in map(Path, sys.argv[1:]):
    for s in sorted(d.glob("*.summary.json")):
        name = s.name[: -len(".summary.json")]
        try:
            summ = json.loads(s.read_text())
        except json.JSONDecodeError:
            continue
        res = [json.loads(l) for p in (d / name).glob("*/results.jsonl") for l in p.read_text().splitlines()]
        ms = sorted(r["ms"] for r in res if r.get("ms") is not None and r["status"] == "ok")
        bad = [f"{r['id']}：{r['status']}" + (f"（{r['error'][:60]}）" if r.get("error") else "") for r in res if r["status"] != "ok"]
        rows[name] = (summ, ms, bad)

print("| 工具 | 版本 | 份数 | 通过 | worker 数 | 启动（ms） | 单份耗时中位数（ms，越低越好） | 单份峰值内存最大（MB，越低越好） | 没通过的 |")
print("|---|---|---|---|---|---|---|---|---|")
for name, (s, ms, bad) in rows.items():
    if s.get("driver_failed"):
        print(f"| {name} | | | driver 失败 | | | | | |")
        continue
    st = s["status"]
    med = f"{ms[len(ms) // 2]:.1f}" if ms else "—"
    start = f"{min(s['startup_ms']):.0f}" if s["startup_ms"] else "—"
    print(f"| {name} | {', '.join(s['versions'])} | {s['items']} | {st.get('ok', 0)} | {s['workers']} | {start} | {med} | {s['max_peak_mb']} | {'；'.join(bad) or '—'} |")
