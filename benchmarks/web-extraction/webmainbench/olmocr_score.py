#!/usr/bin/env python3
"""olmOCR-bench 评分：直接复用官方 olmocr/bench/tests.py 的逐项判定。

官方 benchmark.py 需要 olmocr 全包（含 torch 渲染依赖），这里只取它的判定与聚合逻辑：
  - load_tests() 解析 7 个 jsonl
  - test.run(md_content) -> (passed, explanation)
  - 按 jsonl 文件分组求平均（官方最终分数即各 jsonl 分数的宏平均）
不含 HTML 报告生成（不影响分数）。

用法: olmocr_score.py <bench_data_dir> <candidate_md_dir> <out_dir> [workers]
"""
import json, re, sys, statistics, os, glob
from pathlib import Path
from concurrent.futures import ProcessPoolExecutor

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "benchmarks/pdf/vendor/olmocr"))
from olmocr.bench.tests import load_tests

def _run_one(job):
    t, files = job
    if not files:
        return t, False, "missing md", 0.0
    passes = 0
    expl = ""
    for f in files:
        try:
            ok, e = t.run(Path(f).read_text(encoding="utf-8", errors="replace"))
        except Exception as ex:
            ok, e = False, f"exception: {ex}"
        passes += 1 if ok else 0
        if not ok:
            expl = e
    return t, passes == len(files), expl, passes / len(files)


def main():
    bench = Path(sys.argv[1]); cand = Path(sys.argv[2]); out = Path(sys.argv[3])
    workers = int(sys.argv[4]) if len(sys.argv) > 4 else 64
    out.mkdir(parents=True, exist_ok=True)

    jsonl_files = sorted(bench.glob("*.jsonl"))
    all_tests = []; test_to_jsonl = {}
    for jp in jsonl_files:
        ts = load_tests(str(jp))
        for t in ts:
            test_to_jsonl[t.id] = jp.name
        all_tests.extend(ts)
    print(f"loaded {len(all_tests)} tests from {len(jsonl_files)} files", flush=True)

    # 索引候选输出：{md_base}_pg{page}_repeat{N}.md
    idx = {}
    for f in cand.glob("**/*.md"):
        rel = f.relative_to(cand).as_posix()
        m = re.match(r"^(.*)_pg(\d+)_repeat(\d+)\.md$", rel)
        if m:
            idx.setdefault((m.group(1), int(m.group(2))), []).append(f)

    jobs = []
    for t in all_tests:
        base = os.path.splitext(t.pdf)[0]
        jobs.append((t, [str(f) for f in sorted(idx.get((base, t.page), []))]))

    with ProcessPoolExecutor(max_workers=workers) as pool:
        results = list(pool.map(_run_one, jobs, chunksize=8))

    by_jsonl = {}
    by_type = {}
    rows = []
    for t, ok, expl, ratio in results:
        j = test_to_jsonl.get(t.id, "unknown")
        by_jsonl.setdefault(j, []).append(1.0 if ok else 0.0)
        by_type.setdefault(t.type, []).append(1.0 if ok else 0.0)
        rows.append({"id": t.id, "pdf": t.pdf, "page": t.page, "type": t.type,
                     "jsonl": j, "passed": ok, "ratio": ratio,
                     "explanation": (expl or "")[:400]})

    with (out / "tests.jsonl").open("w", encoding="utf-8") as f:
        for r in rows: f.write(json.dumps(r, ensure_ascii=False) + "\n")

    summary = {
        "total_tests": len(results),
        "passed": sum(1 for r in rows if r["passed"]),
        "overall_pass_rate": statistics.fmean(r["ratio"] for r in rows) if rows else 0.0,
        "per_jsonl": {k: {"n": len(v), "score": statistics.fmean(v)} for k, v in sorted(by_jsonl.items())},
        "per_type": {k: {"n": len(v), "score": statistics.fmean(v)} for k, v in sorted(by_type.items())},
    }
    summary["macro_jsonl_score"] = statistics.fmean(x["score"] for x in summary["per_jsonl"].values())
    (out / "summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2), encoding="utf-8")
    for k, v in summary["per_jsonl"].items():
        print(f"{k:28s} n={v['n']:5d} score={v['score']:.4f}", flush=True)
    print(f"\nMACRO(jsonl) = {summary['macro_jsonl_score']:.4f}   overall={summary['overall_pass_rate']:.4f}   "
          f"passed={summary['passed']}/{summary['total_tests']}")

if __name__ == "__main__":
    main()
