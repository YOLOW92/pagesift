"""配对 bootstrap：新旧两次运行在同一批样本上的逐项差值 Δ = 新 − 旧。
    paired.py <旧 quality-xx.json> <旧 runs 目录> <新 quality-xx.json> <新 runs 目录> <子类>
输出：均值、Δ 的 1−δ 双侧区间和单侧上下界（δ = 0.05），重抽样次数加倍直到端点的蒙特卡洛误差小于统计误差的 1/10。
"""
import json, random, re, statistics, sys
from pathlib import Path
old_rep, old_runs, new_rep, new_runs, sub = sys.argv[1:6]
TOOL = "pagesift-081-file"
D = 0.05
NOTE = re.compile(r"^\*Page \d+ is a scanned image with no text layer\.\*$|^!\[[^\]]*\]\(images/[^)]*\)$")
def load(rep, runs):
    r = json.loads(Path(rep).read_text())["tools"][TOOL]
    res = {}
    for l in (Path(runs) / sub / TOOL / "results.jsonl").read_text().splitlines():
        x = json.loads(l); res[x["id"]] = x
    return r, res
def text_ok(runs, x):
    if not x or x["status"] != "ok": return 0
    md = (Path(runs) / sub / TOOL / f"{x['id']}.md").read_text(errors="replace")
    body = [l for l in md.splitlines() if l.strip() and not NOTE.match(l.strip())]
    return int(bool(body))
o, ores = load(old_rep, old_runs); n, nres = load(new_rep, new_runs)
ids = sorted(set(ores) & set(nres))
metrics = {
  "completion": lambda res, runs, i: int(res[i]["status"] == "ok"),
  "text_completion": lambda res, runs, i: text_ok(runs, res[i]),
}
if sub in ("b2", "b5"):
    po, pn = o["per_item"], n["per_item"]
    metrics["score"] = lambda res, runs, i: (po if runs == old_runs else pn)[i]["f1"]
def boot(d):
    k = len(d); B = 2000; rng = random.Random(1)
    prev = None
    while True:
        ms = sorted(sum(d[rng.randrange(k)] for _ in range(k)) / k for _ in range(B))
        lo, hi = ms[int(D / 2 * B)], ms[int((1 - D / 2) * B) - 1]
        lo1, hi1 = ms[int(D * B)], ms[int((1 - D) * B) - 1]
        se = statistics.pstdev(ms)
        if prev and abs(prev[0] - lo) < se / 10 and abs(prev[1] - hi) < se / 10: return lo, hi, lo1, hi1, B
        prev = (lo, hi); B *= 2
        if B > 64000: return lo, hi, lo1, hi1, B
out = {}
for name, f in metrics.items():
    pairs = []
    for i in ids:
        a, b = f(ores, old_runs, i), f(nres, new_runs, i)
        if a is None or b is None: continue
        pairs.append((a, b))
    d = [b - a for a, b in pairs]
    lo, hi, lo1, hi1, B = boot(d)
    verdict = "提升" if lo1 > 0 else ("不降" if hi1 >= 0 else "下降")
    out[name] = dict(n=len(d), old=statistics.mean(a for a, _ in pairs), new=statistics.mean(b for _, b in pairs),
                     delta=statistics.mean(d), ci95=[lo, hi], one_sided_lo=lo1, one_sided_hi=hi1, B=B, verdict=verdict,
                     changed=sum(1 for x in d if x))
print(json.dumps({"sub": sub, **out}, ensure_ascii=False, indent=1))
