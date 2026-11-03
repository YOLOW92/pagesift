"""九格判定：块预算 100/200/400 × 检索预算 250/500/1000，召回和 IoU，新旧逐题配对 bootstrap。

python -I cells.py <数据目录> <half: all|tune|hold> <旧: 前缀> <新: 前缀> [B]
  前缀 P 表示文件 P-100.json、P-200.json、P-400.json（块区间，{"语料": [[起, 止], ...]}）。
- "不降"：H0 = 新不比旧差。单侧 bootstrap p 值 p = (1 + #{Δ* ≥ 0}) / (B + 1)（Δ* 是重抽样的均值差），
  18 个检验（9 格 × 召回/IoU）一起做 Benjamini–Hochberg，δ = 0.05；被拒绝的判"下降"。
- "提升"：Δ 的双侧 1−δ 区间下界 > 0。
- 重抽样次数从 2000 起翻倍，直到区间端点的蒙特卡洛误差（10 批端点的标准误）小于 bootstrap 标准差的 1/10。
"""
import csv, hashlib, json, os, sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from rag_score import per_question  # noqa: E402

DELTA = 0.05  # [δ] 全局风险水平
SIZES = (100, 200, 400)
BUDGETS = (250, 500, 1000)


def bootstrap(d, rng):
    n = len(d)
    B = 2000
    while True:
        idx = rng.integers(0, n, size=(B, n))
        m = d[idx].mean(axis=1)
        sd = m.std()
        batches = m.reshape(10, -1)
        ends = np.array([[np.quantile(b, DELTA / 2), np.quantile(b, 1 - DELTA / 2), np.quantile(b, 1 - DELTA)] for b in batches])
        mc = ends.std(axis=0, ddof=1).max() / np.sqrt(10)
        if mc < sd / 10 or B >= 64000:
            return m, B
        B *= 2


def main():
    data, half, old_p, new_p = sys.argv[1:5]
    cache = {}

    def scores(path):
        if path not in cache:
            cache[path] = per_question(data, json.load(open(path)))
        return cache[path]

    rows = []
    rng = np.random.default_rng(20261102)
    for s in SIZES:
        old = scores(f"{old_p}-{s}.json")
        new = scores(f"{new_p}-{s}.json")
        keep = np.array([half == "all" or h == (1 if half == "hold" else 0) for h in old["half"]])
        for b in BUDGETS:
            for metric in ("R", "I"):
                o = np.array(old[f"{metric}{b}"])[keep]
                nw = np.array(new[f"{metric}{b}"])[keep]
                d = nw - o
                m, B = bootstrap(d, rng)
                p = (1 + (m >= 0).sum()) / (len(m) + 1)
                rows.append(dict(s=s, b=b, metric=metric, old=o.mean(), new=nw.mean(), d=d.mean(),
                                 lo=np.quantile(m, DELTA / 2), hi=np.quantile(m, 1 - DELTA / 2),
                                 up=np.quantile(m, 1 - DELTA), p=p, B=B, chunks=(old["chunks"], new["chunks"]),
                                 words=(old["words"], new["words"])))
    # Benjamini–Hochberg（"下降"的发现）
    order = sorted(range(len(rows)), key=lambda i: rows[i]["p"])
    m = len(rows)
    k_max = 0
    for rank, i in enumerate(order, 1):
        if rows[i]["p"] <= rank / m * DELTA:
            k_max = rank
    worse = {order[r] for r in range(k_max)}
    print(f"half={half} n={int(keep.sum())}")
    for i, r in enumerate(rows):
        verdict = "下降" if i in worse else ("提升" if r["lo"] > 0 else "不降")
        print(f"S={r['s']:3d} B={r['b']:4d} {'召回' if r['metric'] == 'R' else 'IoU '} 旧 {r['old']:.3f} 新 {r['new']:.3f} "
              f"Δ {r['d']:+.4f} [{r['lo']:+.4f}, {r['hi']:+.4f}] p={r['p']:.4f} B={r['B']} {verdict}"
              + (f"   (块数 {r['chunks'][0]}→{r['chunks'][1]}, 平均词 {r['words'][0]:.0f}→{r['words'][1]:.0f})" if r['b'] == 250 and r['metric'] == 'R' else ""))
    json.dump(rows, open(os.environ.get("CELLS_OUT", os.devnull), "w"), default=float, indent=1)


main()
