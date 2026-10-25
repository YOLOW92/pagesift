#!/usr/bin/env python3
"""WCEB 评分：复用上游 rouge_eval / levenshtein_eval 的确切实现。

ROUGE-LSum(split_summaries=True, use_stemmer=False, 空白分词) + Levenshtein ratio。
空目标按上游规则记 rec=1.0（预览也为空时 prec=f1=1.0）。
"""
import json, sys, statistics, multiprocessing as mp
from pathlib import Path
from rouge_score import rouge_scorer, tokenizers
from Levenshtein import ratio as levenshtein_ratio

DATASETS = ["cetd", "cleaneval", "cleanportaleval", "dragnet",
            "google-trends-2017", "l3s-gn1", "readability", "scrapinghub"]

class Tokenizer(tokenizers.Tokenizer):
    def tokenize(self, text):
        return text.split()

_TOK = Tokenizer()
_ROUGE = rouge_scorer.RougeScorer(["rougeLsum"], use_stemmer=False,
                                  split_summaries=True, tokenizer=_TOK)

def rouge_eval(target, pred):
    s = _ROUGE.score(target, pred)["rougeLsum"]
    p, r, f = s.precision, s.recall, s.fmeasure
    if target.strip() == "":
        r = 1.0
        if pred.strip() == "":
            p = 1.0; f = 1.0
    return p, r, f

def levenshtein_eval(target, pred):
    return levenshtein_ratio(target.split(), pred.split())

_C = None

def _init():
    global _C
    _C = (rouge_eval, levenshtein_eval)

def _one(job):
    text, pred = job
    rouge_eval, levenshtein_eval = _C
    p, r, f = rouge_eval(text, pred)
    return p, r, f, levenshtein_eval(text, pred)

def main():
    combined = Path(sys.argv[1]); out = Path(sys.argv[2]); res = Path(sys.argv[3])
    workers = int(sys.argv[4]) if len(sys.argv) > 4 else 64
    res.mkdir(parents=True, exist_ok=True)
    per_ds = {}
    for ds in DATASETS:
        gt_path = combined / "ground-truth" / f"{ds}.jsonl"
        parts_path = out / f"{ds}.parts.jsonl"
        if not gt_path.exists() or not parts_path.exists():
            continue
        gt = {json.loads(l)["page_id"]: json.loads(l) for l in gt_path.open(encoding="utf-8")}
        preds = {}
        for l in parts_path.open(encoding="utf-8"):
            r = json.loads(l)
            preds[str(r["row"])] = r
        rows = list(gt.values())
        jobs = []
        missing = 0
        mds = []
        for i, row in enumerate(rows):
            pr = preds.get(str(i))
            pred = (pr or {}).get("markdown") or ""
            if pr is None: missing += 1
            mds.append(pred)
            jobs.append((row["plaintext"], pred))
        with mp.get_context("fork").Pool(workers, initializer=_init) as pool:
            results = pool.map(_one, jobs, chunksize=16)
        P = [x[0] for x in results]; R = [x[1] for x in results]
        F = [x[2] for x in results]; L = [x[3] for x in results]
        scored = len(results)
        with (res / f"{ds}.scores.jsonl").open("w", encoding="utf-8") as fh:
            for row, pred, (p, rr, ff, lv) in zip(rows, mds, results):
                fh.write(json.dumps({"page_id": row["page_id"], "source": row.get("source"),
                                     "rouge_p": p, "rouge_r": rr, "rouge_f1": ff,
                                     "levenshtein": lv, "pred_chars": len(pred),
                                     "gt_chars": len(row["plaintext"])}, ensure_ascii=False) + "\n")
        per_ds[ds] = {"n": scored, "missing": missing,
                      "rouge_f1": statistics.fmean(F), "rouge_p": statistics.fmean(P),
                      "rouge_r": statistics.fmean(R), "levenshtein": statistics.fmean(L)}
        print(f"{ds:20s} n={scored:5d} ROUGE-LSum F1={per_ds[ds]['rouge_f1']:.4f} "
              f"P={per_ds[ds]['rouge_p']:.4f} R={per_ds[ds]['rouge_r']:.4f} "
              f"Lev={per_ds[ds]['levenshtein']:.4f}", flush=True)
    allF = [d["rouge_f1"] for d in per_ds.values()]
    allL = [d["levenshtein"] for d in per_ds.values()]
    overall = {"per_dataset": per_ds,
               "overall_rouge_f1_macro": statistics.fmean(allF) if allF else 0.0,
               "overall_levenshtein_macro": statistics.fmean(allL) if allL else 0.0,
               "total_pages": sum(d["n"] for d in per_ds.values())}
    (res / "summary.json").write_text(json.dumps(overall, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps({k: v for k, v in overall.items() if k != "per_dataset"}, indent=2))

if __name__ == "__main__":
    main()
