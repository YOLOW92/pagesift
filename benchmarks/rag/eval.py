"""分块质量：Chroma chunking_evaluation 数据集（5 份语料、472 个问题，每个问题标了答案在原文中的字符区间）。

python -I eval.py <数据目录> <结果目录>   （结果目录里每个 <方法>.json 是 {"语料名": [[起, 止], ...]}）

所有方法用同一个检索器：bm25s（英文停用词）在全部语料的所有块上检索，按分数取块，直到再加一块就超过
token 预算（词数，至少取一块）。在预算 250、500、1000 词下统计（都是越高越好）：
- recall：答案字符被取回内容覆盖的比例；
- precision：取回内容里属于答案的比例（衡量浪费的 token）；
- IoU：交集 / 并集（Chroma 的主指标）。
按问题平均。每类语料单独的召回也一并给出。
"""
import csv, hashlib, json, os, sys

import bm25s

BUDGETS = (250, 500, 1000)


def merge(spans):
    out = []
    for a, b in sorted(spans):
        if out and a <= out[-1][1]:
            out[-1][1] = max(out[-1][1], b)
        else:
            out.append([a, b])
    return out


def length(spans):
    return sum(b - a for a, b in spans)


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


def cut_words(text, a, e, n):
    """从 a 开始取前 n 个词，返回结束位置。"""
    i, count, in_word = a, 0, False
    while i < e:
        ws = text[i].isspace()
        if not ws and not in_word:
            count += 1
            if count > n:
                return i
        in_word = not ws
        i += 1
    return e


def main():
    data, res = sys.argv[1], sys.argv[2]
    texts = {os.path.splitext(f)[0]: open(os.path.join(data, "corpora", f), encoding="utf-8").read()
             for f in os.listdir(os.path.join(data, "corpora"))}
    qs = list(csv.DictReader(open(os.path.join(data, "questions_df.csv"), encoding="utf-8")))
    # 按问题文本的 sha1 分成调参集和保留集（HALF=tune / hold / all）
    half = os.environ.get("HALF", "all")
    if half != "all":
        want = 1 if half == "hold" else 0
        qs = [q for q in qs if int(hashlib.sha1(q["question"].encode()).hexdigest(), 16) % 2 == want]
    print(f"{'method':22} {'chunks':>6} {'words':>6} " + " ".join(f"{'R@'+str(b):>7} {'P@'+str(b):>7} {'IoU@'+str(b):>8}" for b in BUDGETS))
    rows = []
    for f in sorted(os.listdir(res)):
        if not f.endswith(".json"):
            continue
        name = f[:-5]
        spans = json.load(open(os.path.join(res, f)))
        chunks = [(c, a, b) for c, ss in spans.items() for a, b in ss if b > a]
        docs = [texts[c][a:b] for c, a, b in chunks]
        nwords = [len(d.split()) for d in docs]
        retriever = bm25s.BM25()
        retriever.index(bm25s.tokenize(docs, stopwords="en", show_progress=False), show_progress=False)
        qtok = bm25s.tokenize([q["question"] for q in qs], stopwords="en", show_progress=False)
        k = min(len(docs), 60)
        idx, _ = retriever.retrieve(qtok, k=k, show_progress=False)
        acc = {b: [0.0, 0.0, 0.0] for b in BUDGETS}
        for qi, q in enumerate(qs):
            gold = merge([[r["start_index"], r["end_index"]] for r in json.loads(q["references"])])
            corpus = q["corpus_id"]
            for b in BUDGETS:
                got, used, retrieved_chars = [], 0, 0
                for ci in idx[qi]:
                    if used >= b:
                        break
                    c, a, e = chunks[ci]
                    if used + nwords[ci] > b:
                        # 超出预算的块只算预算内的开头部分（大块不能靠"至少取一块"白拿整篇）
                        if got:
                            break
                        e = cut_words(texts[c], a, e, b - used)
                    used += min(nwords[ci], b - used)
                    retrieved_chars += e - a
                    if c == corpus:
                        got.append([a, e])
                mine = merge(got)
                hit = inter(mine, gold)
                acc[b][0] += hit / length(gold)
                acc[b][1] += hit / retrieved_chars if retrieved_chars else 0
                acc[b][2] += hit / (retrieved_chars + length(gold) - hit) if retrieved_chars else 0
        n = len(qs)
        row = f"{name:22} {len(docs):6d} {sum(nwords) / len(docs):6.0f} " + " ".join(
            f"{acc[b][0] / n:7.3f} {acc[b][1] / n:7.3f} {acc[b][2] / n:8.3f}" for b in BUDGETS)
        rows.append((acc[1000][0] / n, row))
    for _, r in sorted(rows, reverse=True):
        print(r)


main()
