"""逐题评分：检索和截断规则与 benchmarks/rag/eval.py 完全相同（bm25s、英文停用词、按词数预算取块），
返回每个问题在每个预算下的召回、精确率、IoU。

作为脚本：python -I rag_score.py <数据目录> <块区间.json> <输出.json>
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


def per_question(data, spans):
    texts = {os.path.splitext(f)[0]: open(os.path.join(data, "corpora", f), encoding="utf-8").read()
             for f in os.listdir(os.path.join(data, "corpora"))}
    qs = list(csv.DictReader(open(os.path.join(data, "questions_df.csv"), encoding="utf-8")))
    chunks = [(c, a, b) for c, ss in spans.items() for a, b in ss if b > a]
    docs = [texts[c][a:b] for c, a, b in chunks]
    nwords = [len(d.split()) for d in docs]
    retriever = bm25s.BM25()
    retriever.index(bm25s.tokenize(docs, stopwords="en", show_progress=False), show_progress=False)
    qtok = bm25s.tokenize([q["question"] for q in qs], stopwords="en", show_progress=False)
    k = min(len(docs), 60)
    idx, _ = retriever.retrieve(qtok, k=k, show_progress=False)
    res = {"q": [], "half": [], "corpus": [], "chunks": len(docs), "words": sum(nwords) / len(docs)}
    for b in BUDGETS:
        for m in "RPI":
            res[f"{m}{b}"] = []
    for qi, q in enumerate(qs):
        h = hashlib.sha1(q["question"].encode()).hexdigest()
        res["q"].append(h)
        # eval.py 的 HALF：sha1 为奇数的是保留集（hold），偶数是调参集（tune）
        res["half"].append(int(h, 16) % 2)
        res["corpus"].append(q["corpus_id"])
        gold = merge([[r["start_index"], r["end_index"]] for r in json.loads(q["references"])])
        corpus = q["corpus_id"]
        for b in BUDGETS:
            got, used, retrieved_chars = [], 0, 0
            for ci in idx[qi]:
                if used >= b:
                    break
                c, a, e = chunks[ci]
                if used + nwords[ci] > b:
                    if got:
                        break
                    e = cut_words(texts[c], a, e, b - used)
                used += min(nwords[ci], b - used)
                retrieved_chars += e - a
                if c == corpus:
                    got.append([a, e])
            mine = merge(got)
            hit = inter(mine, gold)
            res[f"R{b}"].append(hit / length(gold))
            res[f"P{b}"].append(hit / retrieved_chars if retrieved_chars else 0)
            res[f"I{b}"].append(hit / (retrieved_chars + length(gold) - hit) if retrieved_chars else 0)
    return res


if __name__ == "__main__":
    data, spans_path, out_path = sys.argv[1:4]
    json.dump(per_question(data, json.load(open(spans_path))), open(out_path, "w"))
