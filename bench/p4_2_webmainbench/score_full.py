#!/usr/bin/env python3
"""Score PageSift outputs with the pinned MinerU-HTML WebMainBench ROUGE-5 function.

Only PageSift is executed. Dataset and output JSONL must have the same row order.
All rows, including empty outputs and errors, remain in the denominator.
"""

import argparse
import json
import multiprocessing as mp
import statistics
import sys
from collections import defaultdict
from itertools import zip_longest
from pathlib import Path

calc_rouge_n_score = None


def init_worker(mineru_dir: str) -> None:
    global calc_rouge_n_score
    sys.path.insert(0, mineru_dir)
    from eval_baselines.utils import calc_rouge_n_score as scorer
    calc_rouge_n_score = scorer


def score_one(item: tuple[int, dict, dict]) -> dict:
    i, gold, pred = item
    ident = gold["track_id"]
    if pred.get("track_id") != ident:
        raise ValueError(f"row {i}: ID mismatch: {ident} != {pred.get('track_id')}")
    target = gold.get("convert_main_content") or ""
    markdown = pred.get("markdown") or ""
    scores = calc_rouge_n_score(target, markdown)
    return {
        "row": i,
        "track_id": ident,
        "status": pred.get("status", "missing"),
        "empty": not bool(markdown.strip()),
        "target_chars": len(target),
        "prediction_chars": len(markdown),
        "extract_ms": pred.get("extract_ms"),
        "prec": scores["prec"],
        "rec": scores["rec"],
        "f1": scores["f1"],
        "meta": gold.get("meta") or {},
    }


def pairs(data: Path, output: Path):
    with data.open(encoding="utf-8") as df, output.open(encoding="utf-8") as pf:
        for i, (dline, pline) in enumerate(zip_longest(df, pf)):
            if dline is None or pline is None:
                raise ValueError(f"dataset/output line count differs at row {i}")
            yield i, json.loads(dline), json.loads(pline)


def summarize(rows: list[dict]) -> dict:
    def mean(key: str, subset: list[dict]) -> float:
        return statistics.fmean(x[key] for x in subset) if subset else 0.0

    summary = {
        "n": len(rows),
        "f1": mean("f1", rows),
        "precision": mean("prec", rows),
        "recall": mean("rec", rows),
        "empty": sum(x["empty"] for x in rows),
        "errors": sum(x["status"] != "ok" for x in rows),
        "mean_extract_ms": statistics.fmean(x["extract_ms"] for x in rows if x["extract_ms"] is not None),
        "groups": {},
    }
    for field in ("level", "language", "style", "code", "table", "equation"):
        groups = defaultdict(list)
        for row in rows:
            groups[str(row["meta"].get(field, "unknown"))].append(row)
        summary["groups"][field] = {
            value: {"n": len(subset), "f1": mean("f1", subset)}
            for value, subset in sorted(groups.items())
        }
    return summary


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("dataset", type=Path)
    parser.add_argument("predictions", type=Path)
    parser.add_argument("mineru_dir", type=Path)
    parser.add_argument("out_dir", type=Path)
    parser.add_argument("--workers", type=int, default=48)
    args = parser.parse_args()
    args.out_dir.mkdir(parents=True, exist_ok=True)
    ctx = mp.get_context("fork")
    with ctx.Pool(args.workers, initializer=init_worker, initargs=(str(args.mineru_dir.resolve()),)) as pool:
        rows = list(pool.imap_unordered(score_one, pairs(args.dataset, args.predictions), chunksize=8))
    rows.sort(key=lambda row: row["row"])
    with (args.out_dir / "scores.jsonl").open("w", encoding="utf-8") as f:
        for row in rows:
            f.write(json.dumps(row, ensure_ascii=False) + "\n")
    summary = summarize(rows)
    (args.out_dir / "summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps({k: v for k, v in summary.items() if k != "groups"}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
