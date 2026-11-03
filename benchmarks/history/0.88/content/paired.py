"""A1 四套集新旧逐页配对比较。只用标准库。

    python3 -I paired.py <旧 quality-a1.json> <新 quality-a1.json> [旧工具名 新工具名]

两份 json 都由 benchmarks/mix/score_quality.py 生成（逐份分数在 tools.<工具>.per_item）。
- 题目分两半：id 的 SHA-256 首字节为偶数的归“调参半”（分析漏掉的块只看这一半），奇数归“保留半”
  （只用来验证）；另报全部 150 页。
- 指标与 0.81 质量轮相同：AEB 用 evaluate.py 的合并 F1（逐份 tp/fp/fn 加总后再算），其余三套是逐份平均
  （Firecrawl 为正文召回，WCXB、WebMainBench 为词级 F1）。全部越高越好。
- Δ = 新 − 旧，逐页配对 bootstrap（同一组重抽样的页同时算新旧两边），δ = 0.05：
  双侧 95% 区间；“提升” = BH 校正后单侧显著且区间下界 > 0；“不降” = 单侧 95% 上界 ≥ 0。
  多套集一起判“提升”时用 Benjamini–Hochberg（单侧 bootstrap p 值 = Δ* ≤ 0 的比例）。
- 重抽样次数：从 2 000 起每次翻倍，直到两个区间端点的蒙特卡洛误差（端点附近 ±√(Bp(1−p)) 个次序统计量
  的跨度之半）不超过区间宽度的 1/20，上限 64 000。固定种子。
"""
import hashlib
import json
import random
import sys

DELTA = 0.05  # 显著性水平 δ
SETS = ["Firecrawl", "WebMainBench", "AEB", "WCXB"]


def half(id_):
    return "tune" if hashlib.sha256(id_.encode()).digest()[0] % 2 == 0 else "hold"


def pooled_f1(rows):
    tp = sum(r["tp_fp_fn"][0] for r in rows)
    fp = sum(r["tp_fp_fn"][1] for r in rows)
    fn = sum(r["tp_fp_fn"][2] for r in rows)
    return 2 * tp / (2 * tp + fp + fn) if tp else 0.0


def stat(set_, rows):
    if set_ == "AEB":
        return pooled_f1(rows)
    return sum(r["f1"] for r in rows) / len(rows)


def boot(set_, pairs, seed):
    """pairs: [(旧行, 新行)]。返回 (旧, 新, Δ, 下界, 上界, 单侧上界, p_提升, B)。"""
    rng = random.Random(seed)
    n = len(pairs)
    o = stat(set_, [p[0] for p in pairs])
    w = stat(set_, [p[1] for p in pairs])
    ds = []
    b = 2000
    while True:
        while len(ds) < b:
            idx = [rng.randrange(n) for _ in range(n)]
            ds.append(stat(set_, [pairs[i][1] for i in idx]) - stat(set_, [pairs[i][0] for i in idx]))
        s = sorted(ds)
        q = lambda p: s[min(len(s) - 1, max(0, int(p * len(s))))]
        lo, hi = q(DELTA / 2), q(1 - DELTA / 2)
        k = max(1, int((len(s) * (DELTA / 2) * (1 - DELTA / 2)) ** 0.5)) / len(s)
        mc = max(q(DELTA / 2 + k) - q(DELTA / 2 - k), q(1 - DELTA / 2 + k) - q(1 - DELTA / 2 - k)) / 2
        if mc <= (hi - lo) / 20 or len(s) >= 64000:
            break
        b *= 2
    one = q(1 - DELTA)
    p = sum(1 for d in s if d <= 0) / len(s)
    return o, w, w - o, lo, hi, one, p, len(s)


def bh(ps):
    """Benjamini–Hochberg：返回在 δ 水平上被拒绝（判“提升”）的下标集合。"""
    order = sorted(range(len(ps)), key=lambda i: ps[i])
    m, k = len(ps), 0
    for r, i in enumerate(order, 1):
        if ps[i] <= DELTA * r / m:
            k = r
    return {order[r] for r in range(k)}


def main():
    a = sys.argv[1:]
    old, new = (json.load(open(p, encoding="utf-8")) for p in a[:2])
    to = a[2] if len(a) > 2 else next(iter(old["tools"]))
    tn = a[3] if len(a) > 3 else next(iter(new["tools"]))
    po, pn = old["tools"][to]["per_item"], new["tools"][tn]["per_item"]
    print(f"# A1 新旧逐页配对（Δ = 新 − 旧，δ = {DELTA}，指标全部越高越好）\n")
    print("| 集 | 半 | 页数 | 旧 | 新 | Δ | 95% 区间 | 单侧 95% 上界 | 判定 | B |")
    print("|---|---|---:|---:|---:|---:|---|---:|---|---:|")
    out = []
    for part in ("tune", "hold", "all"):
        res = []
        for si, set_ in enumerate(SETS):
            ids = sorted(i for i, r in po.items() if r["set"] == set_ and i in pn and (part == "all" or half(i) == part))
            res.append((set_, len(ids), boot(set_, [(po[i], pn[i]) for i in ids], seed=88 + si)))
        rej = bh([r[6] for _, _, r in res])
        for k, (set_, n, (o, w, d, lo, hi, one, p, b)) in enumerate(res):
            verdict = "提升" if k in rej and lo > 0 else ("不降" if one >= 0 else "下降")
            name = {"tune": "调参半", "hold": "保留半", "all": "全部"}[part]
            print(f"| {set_} | {name} | {n} | {o:.4f} | {w:.4f} | {d:+.4f} | [{lo:+.4f}, {hi:+.4f}] | {one:+.4f} | {verdict} | {b} |")
            out.append({"set": set_, "half": part, "n": n, "old": o, "new": w, "delta": d, "lo": lo, "hi": hi,
                        "upper_one_sided": one, "p_improve": p, "verdict": verdict, "B": b})
    json.dump(out, open(a[1].replace(".json", ".paired.json"), "w", encoding="utf-8"), ensure_ascii=False, indent=1)


if __name__ == "__main__":
    main()
