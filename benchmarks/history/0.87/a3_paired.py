"""A3 新旧逐页配对比较。只用标准库。

    python3 -I a3_paired.py <参照 1> <参照 2> <旧运行目录> <新运行目录>

逐页指标（同 benchmarks/mix/v2/score_a3.py）：
- 完成：状态 ok 且输出至少 200 字；
- 参照词覆盖率：两次 Firefox 渲染都出现的词里，出现在输出中的比例；没完成按 0；
- 耗时：driver 记的单页毫秒数（只算两边都完成的页）。
差值 Δ = 新 − 旧，逐页配对 bootstrap（重抽样 10 000 次，固定种子），给 95% 区间。
"""
import json
import random
import re
import statistics
import sys
from pathlib import Path

WORD = re.compile(r"\w+", re.UNICODE)
B = 10_000  # 重抽样次数；区间端点的蒙特卡洛误差远小于报告的位数


def words(text):
    return set(w.lower() for w in WORD.findall(text))


def load(run):
    rows = {}
    for line in (run / "results.jsonl").read_text().splitlines():
        r = json.loads(line)
        rows[r["id"]] = r
    return rows


def text(run, i):
    p = run / f"{i}.md"
    return p.read_text(encoding="utf-8", errors="replace") if p.exists() else ""


def boot(d, rng):
    n = len(d)
    means = sorted(statistics.fmean(d[rng.randrange(n)] for _ in range(n)) for _ in range(B))
    return means[int(0.025 * B)], means[int(0.975 * B) - 1]


def main():
    ref1, ref2, old, new = (Path(a) for a in sys.argv[1:5])
    ro, rn = load(old), load(new)
    ids = sorted(set(ro) | set(rn))
    ref = {}
    for i in ids:
        a, b = ref1 / f"{i}.txt", ref2 / f"{i}.txt"
        if a.exists() and b.exists():
            w = words(a.read_text(encoding="utf-8", errors="replace")) & words(b.read_text(encoding="utf-8", errors="replace"))
            if w:
                ref[i] = w

    def done(rows, run, i):
        return rows.get(i, {}).get("status") == "ok" and len(text(run, i)) >= 200

    rng = random.Random(87)
    comp = [(int(done(rn, new, i)) - int(done(ro, old, i))) for i in ids]
    cov = []
    for i in ref:
        co = len(ref[i] & words(text(old, i))) / len(ref[i]) if done(ro, old, i) else 0.0
        cn = len(ref[i] & words(text(new, i))) / len(ref[i]) if done(rn, new, i) else 0.0
        cov.append((i, co, cn))
    both = [i for i in ids if done(ro, old, i) and done(rn, new, i)]
    ms = [(rn[i]["ms"] - ro[i]["ms"]) / 1000 for i in both if rn[i].get("ms") is not None and ro[i].get("ms") is not None]

    print("# A3：新旧逐页配对（Δ = 新 − 旧，95% 区间来自逐页配对 bootstrap）\n")
    print(f"共 {len(ids)} 页；两次参照渲染都拿到文字的 {len(ref)} 页；两边都完成的 {len(both)} 页。\n")
    print("| 指标 | 旧 | 新 | Δ | 95% 区间 |")
    print("|---|---:|---:|---:|---|")
    co = statistics.fmean(c for _, c, _ in cov)
    cn = statistics.fmean(c for _, _, c in cov)
    d = [n - o for _, o, n in cov]
    lo, hi = boot(d, rng)
    print(f"| 参照词覆盖率 ↑（没完成按 0） | {co:.3f} | {cn:.3f} | {cn - co:+.3f} | [{lo:+.3f}, {hi:+.3f}] |")
    done_o = sum(done(ro, old, i) for i in ids) / len(ids)
    done_n = sum(done(rn, new, i) for i in ids) / len(ids)
    lo, hi = boot(comp, rng)
    print(f"| 完成率 ↑ | {done_o:.3f} | {done_n:.3f} | {done_n - done_o:+.3f} | [{lo:+.3f}, {hi:+.3f}] |")
    if ms:
        lo, hi = boot(ms, rng)
        po = statistics.median(ro[i]["ms"] for i in both) / 1000
        pn = statistics.median(rn[i]["ms"] for i in both) / 1000
        print(f"| 单页耗时 s ↓（两边都完成的页，中位数；Δ 为逐页差的均值） | {po:.2f} | {pn:.2f} | {statistics.fmean(ms):+.2f} | [{lo:+.2f}, {hi:+.2f}] |")
    worse = sorted(((n - o), i) for i, o, n in cov if n < o)
    if worse:
        print("\n覆盖率下降的页（Δ 最大的 10 个）：\n")
        for delta, i in worse[:10]:
            print(f"- {i}：{delta:+.3f}")


if __name__ == "__main__":
    main()
