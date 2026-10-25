#!/usr/bin/env python3
"""D6 评分：复用 0.50 eval_d6_050.py 的 canon()/is_compat()/categorize() 口径。"""
import json, statistics, sys
from pathlib import Path
ROOT = Path("/home/yolow92/pagesift")
sys.path.insert(0, str(ROOT / "bench/fresh"))
import importlib.util
spec = importlib.util.spec_from_file_location("d6", ROOT / "bench/fresh/eval_d6_050.py")
d6 = importlib.util.module_from_spec(spec)
import types
# 只取评分函数，不跑 main
src = (ROOT / "bench/fresh/eval_d6_050.py").read_text()
ns = {}
exec(compile(src.split("# ----------------- Competitor Workers")[0], "d6", "exec"), ns)
is_compat, categorize = ns["is_compat"], ns["categorize"]

man = [json.loads(l) for l in (ROOT / "bench/data/showdown/d6/manifest.jsonl").open()]

def score_tsv(p):
    pred = {}
    for line in p.open(encoding="utf-8"):
        parts = line.rstrip("\n").split("\t")
        if len(parts) == 2:
            pred[parts[0]] = parts[1]
    return pred

def score_jsonl(p):
    pred = {}
    for line in p.open(encoding="utf-8"):
        r = json.loads(line)
        pred[r["id"]] = r["detected"]
    return pred

def evaluate(pred):
    ok = []; bycat = {}
    for m in man:
        d = pred.get(m["id"]) or pred.get(m["path"])
        good = is_compat(d, m["expected"])
        ok.append(1.0 if good else 0.0)
        bycat.setdefault(categorize(m["expected"]), []).append(1.0 if good else 0.0)
    return {"n": len(ok), "accuracy": statistics.fmean(ok),
            "by_category": {k: {"n": len(v), "acc": statistics.fmean(v)} for k, v in sorted(bycat.items())}}

res = {}
for name, path, fn in [
    ("pagesift-070", ROOT/"out/d6-070.tsv", score_tsv),
    ("pagesift-050", ROOT/"out/d6-050.tsv", score_tsv),
    ("chardet", ROOT/"out/d6-chardet.jsonl", score_jsonl),
    ("charset-normalizer", ROOT/"out/d6-charset-normalizer.jsonl", score_jsonl),
    ("cchardet", ROOT/"out/d6-cchardet.jsonl", score_jsonl),
]:
    if path.exists():
        res[name] = evaluate(fn(path))

(ROOT/"out/d6/score.json").parent.mkdir(parents=True, exist_ok=True)
(ROOT/"out/d6/score.json").write_text(json.dumps(res, ensure_ascii=False, indent=2))
for k, v in sorted(res.items(), key=lambda kv: -kv[1]["accuracy"]):
    print(f"{k:<22}{v['accuracy']:>9.4f}{v['n']:>7}")
print()
for cat in sorted(res["pagesift-070"]["by_category"]):
    a = res["pagesift-070"]["by_category"][cat]
    b = res["pagesift-050"]["by_category"][cat]
    print(f"{cat:<42}{b['acc']:>8.4f}{a['acc']:>8.4f}{a['acc']-b['acc']:>+9.4f}{a['n']:>6}")
