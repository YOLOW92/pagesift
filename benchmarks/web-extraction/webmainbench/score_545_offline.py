#!/usr/bin/env python3
"""Run pinned WebMainBench five-part metric calculator with use_llm=False.

The published 545-sample leaderboard uses LLM-enhanced splitting. This offline
diagnostic deliberately does not claim to reproduce that leaderboard.
"""

import argparse
import json
import multiprocessing as mp
import statistics
import sys
import types
from itertools import zip_longest
from pathlib import Path

calculator = None
METRICS = ("text_edit", "code_edit", "formula_edit", "table_edit", "table_TEDS", "overall")


def init_worker(benchmark_dir: str) -> None:
    global calculator
    package = types.ModuleType("webmainbench")
    package.__path__ = [str(Path(benchmark_dir) / "webmainbench")]
    sys.modules["webmainbench"] = package
    from webmainbench.metrics.calculator import MetricCalculator
    calculator = MetricCalculator({"use_llm": False})


def score_one(item: tuple[int, dict, dict]) -> dict:
    i, gold, pred = item
    ident = gold["track_id"]
    if pred.get("track_id") != ident:
        raise ValueError(f"row {i}: ID mismatch")
    markdown = pred.get("markdown") or ""
    reference = gold.get("groundtruth_content") or ""
    try:
        calculated = calculator.calculate_all(
            predicted_content=markdown,
            groundtruth_content=reference,
            predicted_content_list=[],
            groundtruth_content_list=None,
        )
        metrics = {
            key: {"score": value.score, "success": value.success, "error": value.error_message}
            for key, value in calculated.items()
        }
        error = None
    except Exception as exc:
        metrics = {}
        error = str(exc)
    return {
        "row": i, "track_id": ident, "status": pred.get("status", "missing"),
        "empty": not bool(markdown.strip()), "extract_ms": pred.get("extract_ms"),
        "metrics": metrics, "scorer_error": error, "meta": gold.get("meta") or {},
    }


def pairs(data: Path, output: Path):
    with data.open(encoding="utf-8") as df, output.open(encoding="utf-8") as pf:
        for i, (dline, pline) in enumerate(zip_longest(df, pf)):
            if dline is None or pline is None:
                raise ValueError(f"dataset/output line count differs at row {i}")
            yield i, json.loads(dline), json.loads(pline)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("dataset", type=Path)
    parser.add_argument("predictions", type=Path)
    parser.add_argument("benchmark_dir", type=Path)
    parser.add_argument("out_dir", type=Path)
    parser.add_argument("--workers", type=int, default=16)
    args = parser.parse_args()
    args.out_dir.mkdir(parents=True, exist_ok=True)
    with mp.get_context("fork").Pool(
        args.workers, initializer=init_worker, initargs=(str(args.benchmark_dir.resolve()),)
    ) as pool:
        rows = list(pool.imap_unordered(score_one, pairs(args.dataset, args.predictions), chunksize=2))
    rows.sort(key=lambda row: row["row"])
    with (args.out_dir / "scores.jsonl").open("w", encoding="utf-8") as out:
        for row in rows:
            out.write(json.dumps(row, ensure_ascii=False) + "\n")
    summary = {
        "mode": "official metric calculator, use_llm=False; not comparable to published LLM-assisted leaderboard",
        "n": len(rows),
        "extract_errors": sum(row["status"] != "ok" for row in rows),
        "scorer_errors": sum(row["scorer_error"] is not None for row in rows),
        "empty": sum(row["empty"] for row in rows),
        "metrics": {},
    }
    for metric in METRICS:
        values = [row["metrics"][metric]["score"] for row in rows
                  if metric in row["metrics"] and row["metrics"][metric]["success"]]
        summary["metrics"][metric] = {
            "mean": statistics.fmean(values) if values else 0.0,
            "scored": len(values),
            "total": len(rows),
        }
    core = ["text_edit", "code_edit", "formula_edit", "table_edit", "table_TEDS"]
    summary["global_overall_five_mean"] = statistics.fmean(summary["metrics"][x]["mean"] for x in core)
    (args.out_dir / "summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps(summary, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
