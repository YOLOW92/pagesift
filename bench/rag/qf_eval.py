"""queryFilter 评测：在每个问题所属的文档里筛选相关内容。
python -I qf_eval.py <语料目录> <问题.jsonl> <结果.json>...   （HALF=tune/hold/all）

结果里的方法名带 "-default" 的是"原样输出"（保留的区间，按原文顺序）：报告召回率、输出词数、精确率。
其余是排好序的区间：按分数取到 100、250、500 词预算，报告召回率和精确率（越高越好）。"""
import json, os, sys

BUDGETS = (100, 250, 500)


def merge(spans):
    out = []
    for a, b in sorted(spans):
        if out and a <= out[-1][1]:
            out[-1][1] = max(out[-1][1], b)
        else:
            out.append([a, b])
    return out


def inter(x, y):
    i = j = n = 0
    while i < len(x) and j < len(y):
        a, b = max(x[i][0], y[j][0]), min(x[i][1], y[j][1])
        if a < b:
            n += b - a
        if x[i][1] < y[j][1]:
            i += 1
        else:
            j += 1
    return n


def cut(text, a, e, n):
    i, count, inw = a, 0, False
    while i < e:
        ws = text[i].isspace()
        if not ws and not inw:
            count += 1
            if count > n:
                return i
        inw = not ws
        i += 1
    return e


def main():
    d, qpath = sys.argv[1], sys.argv[2]
    half = os.environ.get("HALF", "all")
    texts = {f[:-3]: open(os.path.join(d, f), encoding="utf-8").read() for f in os.listdir(d)}
    qs = [json.loads(l) for l in open(qpath)]
    qs = [q for q in qs if half == "all" or q["half"] == half]
    methods = {}
    for p in sys.argv[3:]:
        methods.update(json.load(open(p)))
    print(f"{len(qs)} questions ({half})")
    print(f"{'method':22} " + " ".join(f"{'R@'+str(b):>7} {'P@'+str(b):>7}" for b in BUDGETS) + f" {'R(kept)':>8} {'words':>7} {'P(kept)':>8}")
    for name, per in sorted(methods.items()):
        acc = {b: [0.0, 0.0] for b in BUDGETS}
        kr = kw = kp = 0.0
        for q in qs:
            t = texts[q["corpus"]]
            gold = merge(q["gold"])
            glen = sum(b - a for a, b in gold)
            spans = per.get(q["id"], [])
            if name.endswith("-default"):
                m = merge(spans)
                hit = inter(m, gold)
                size = sum(b - a for a, b in m)
                kr += hit / glen
                kw += sum(len(t[a:b].split()) for a, b in m)
                kp += hit / size if size else 0
                continue
            for b in BUDGETS:
                got, used = [], 0
                for a, e in spans:
                    if used >= b:
                        break
                    n = len(t[a:e].split())
                    if used + n > b:
                        if got:
                            break
                        e = cut(t, a, e, b - used)
                        n = b - used
                    used += n
                    got.append([a, e])
                m = merge(got)
                hit = inter(m, gold)
                size = sum(e - a for a, e in m)
                acc[b][0] += hit / glen
                acc[b][1] += hit / size if size else 0
        n = len(qs)
        if name.endswith("-default"):
            print(f"{name:22} " + " ".join(f"{'':>7} {'':>7}" for _ in BUDGETS) + f" {kr / n:8.3f} {kw / n:7.0f} {kp / n:8.3f}")
        else:
            print(f"{name:22} " + " ".join(f"{acc[b][0] / n:7.3f} {acc[b][1] / n:7.3f}" for b in BUDGETS))


main()
