#!/usr/bin/env python3
"""D7 评分：标题/作者/发布日期三项抽取准确率。
判定：归一化后互contains即算对（标题常带站点名后缀）；日期只比到日。"""
import json, re, statistics
from pathlib import Path
ROOT = Path("/home/yolow92/pagesift")
man = [json.loads(l) for l in (ROOT/"benchmarks/data/showdown/d7/manifest.jsonl").open()]

def norm(s):
    if not s: return ""
    s = re.sub(r"[\s\u3000]+", " ", str(s)).strip().lower()
    return re.sub(r"[^\w\u4e00-\u9fff ]+", "", s)

def date_only(s):
    if not s: return ""
    m = re.search(r"(\d{4})[-/](\d{2})[-/](\d{2})", str(s))
    return f"{m.group(1)}-{m.group(2)}-{m.group(3)}" if m else ""

def ok_text(pred, gold):
    if not pred or not gold: return False
    a, b = norm(pred), norm(gold)
    if not a or not b: return False
    return a == b or b in a or a in b

def load_jsonl(p):
    d = {}
    for line in p.open(encoding="utf-8"):
        r = json.loads(line)
        k = r["id"].split("/")[-1] if "/" in r["id"] else r["id"]
        d[k] = r
    return d

def evaluate(pred):
    t = []; a = []; p = []
    for m in man:
        r = pred.get(m["id"])
        if m.get("gt_title"): t.append(1.0 if r and ok_text(r.get("title"), m["gt_title"]) else 0.0)
        if m.get("gt_author"): a.append(1.0 if r and ok_text(r.get("author"), m["gt_author"]) else 0.0)
        if m.get("gt_publish"):
            p.append(1.0 if r and date_only(r.get("published")) and date_only(r.get("published"))==date_only(m["gt_publish"]) else 0.0)
    return {"title": {"n": len(t), "acc": statistics.fmean(t) if t else 0.0},
            "author": {"n": len(a), "acc": statistics.fmean(a) if a else 0.0},
            "published": {"n": len(p), "acc": statistics.fmean(p) if p else 0.0},
            "mean3": statistics.fmean([statistics.fmean(t) if t else 0,
                                       statistics.fmean(a) if a else 0,
                                       statistics.fmean(p) if p else 0])}

res = {}
for name, f in [("pagesift-070","out/d7-070.jsonl"),("pagesift-050","out/d7-050.jsonl"),
                ("extruct","out/d7-extruct.jsonl"),("bs4lxml","out/d7-bs4lxml.jsonl")]:
    pth = ROOT/f
    if pth.exists(): res[name] = evaluate(load_jsonl(pth))

(ROOT/"out/d7").mkdir(exist_ok=True)
(ROOT/"out/d7/score.json").write_text(json.dumps(res, ensure_ascii=False, indent=2))
print(f"{'参赛者':<16}{'标题':>8}{'作者':>8}{'日期':>8}{'三项均值':>10}")
for k,v in sorted(res.items(), key=lambda kv:-kv[1]["mean3"]):
    print(f"{k:<16}{v['title']['acc']:>8.4f}{v['author']['acc']:>8.4f}{v['published']['acc']:>8.4f}{v['mean3']:>10.4f}")
print("\n样本量:", {k:(v['title']['n'],v['author']['n'],v['published']['n']) for k,v in res.items()})
