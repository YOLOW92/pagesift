#!/usr/bin/env python3
"""D5 分块+BM25 竞品适配器：semchunk / langchain-text-splitters / chonkie。
统一 512 token 目标块大小，BM25 用 rank_bm25，与 PageSift 同口径算 Recall@k。"""
import argparse, json, re, sys
from pathlib import Path

def split_semchunk(text):
    import semchunk
    return semchunk.chunk(text, chunk_size=512, token_counter=lambda t: len(t.split()))

def split_langchain(text):
    from langchain_text_splitters import RecursiveCharacterTextSplitter
    return RecursiveCharacterTextSplitter(chunk_size=2048, chunk_overlap=200,
                                          length_function=len).split_text(text)

def split_chonkie(text):
    from chonkie import TokenChunker
    return [c.text for c in TokenChunker(chunk_size=512).chunk(text)]

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tool", required=True)
    ap.add_argument("--questions", required=True)
    ap.add_argument("--corpora", required=True)
    ap.add_argument("--k", type=int, default=5)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    fn = {"semchunk": split_semchunk, "langchain": split_langchain, "chonkie": split_chonkie}[a.tool]
    from rank_bm25 import BM25Okapi

    qs = [json.loads(l) for l in open(a.questions, encoding="utf-8") if l.strip()]
    cache = {}
    hits = total = 0
    by = {}
    t_import = None
    for q in qs:
        cid = q["corpus_id"]
        if cid not in cache:
            text = Path(a.corpora, f"{cid}.md").read_text(encoding="utf-8")
            chunks = fn(text)
            # 记录每块在原文里的位置，用于命中判定
            spans, pos = [], 0
            for c in chunks:
                i = text.find(c, pos)
                if i < 0: i = pos
                spans.append((i, i + len(c)))
                pos = i + len(c)
            toks = [re.findall(r"\w+", c.lower()) for c in chunks]
            cache[cid] = (BM25Okapi(toks), spans)
        bm, spans = cache[cid]
        refs = [(r["start_index"], r["end_index"]) for r in q.get("references") or []]
        if not refs: continue
        scores = bm.get_scores(re.findall(r"\w+", q["question"].lower()))
        top = sorted(range(len(scores)), key=lambda i: -scores[i])[:a.k]
        ok = False
        for ci in top:
            s, e = spans[ci]
            for (rs, re_) in refs:
                lo, hi = max(s, rs), min(e, re_)
                if s <= rs and e >= re_: ok = True; break
                if hi > lo and (hi - lo) >= (re_ - rs) * 0.8: ok = True; break
            if ok: break
        total += 1
        if ok: hits += 1
        x = by.setdefault(cid, [0, 0]); x[1] += 1
        if ok: x[0] += 1
    out = {"tool": a.tool, "k": a.k, "questions": total, "hits": hits,
           "recall_at_k": hits / total if total else 0.0,
           "by_corpus": [{"corpus": c, "hits": v[0], "total": v[1], "recall": v[0]/v[1]} for c, v in by.items()]}
    Path(a.out).write_text(json.dumps(out, ensure_ascii=False, indent=2))
    print(json.dumps({"tool": a.tool, "recall": round(out["recall_at_k"], 4)}), file=sys.stderr)

if __name__ == "__main__":
    main()
