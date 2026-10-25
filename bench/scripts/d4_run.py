#!/usr/bin/env python3
"""D4 DOM 自愈批量评测：逐用例调用 pagesift relocate，统计召回/误配/延迟。

判定与 0.50 一致：relocate 返回的 text 与 expected 互相包含即算命中；
返回 status=abstain（弃权）或空文本记为未找到。
"""
import json, statistics, subprocess, sys, time
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

BIN = None; CASES = None

def init(bin_path):
    global BIN
    BIN = bin_path

def work(c):
    t0 = time.perf_counter()
    try:
        r = subprocess.run([BIN, "relocate", "--original", c["orig"],
                            "--selector", c["selector"], c["mut"]],
                           capture_output=True, text=True, timeout=30)
        us = (time.perf_counter() - t0) * 1e6
        out = json.loads(r.stdout) if r.stdout.strip() else {}
    except Exception:
        return {"id": c["id"], "category": c["category"], "found": False,
                "ok": False, "status": "error", "us": 0.0}
    text = (out.get("text") or "").strip()
    status = out.get("status") or "unknown"
    exp = (c["expected"] or "").strip()
    ok = bool(text) and (exp in text or text in exp)
    return {"id": c["id"], "category": c["category"],
            "found": bool(text), "ok": ok, "status": status,
            "score": out.get("score"), "us": us}

def main():
    bin_path, case_file, out_path, workers = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])
    cases = [json.loads(l) for l in open(case_file, encoding="utf-8") if l.strip()]
    t0 = time.perf_counter()
    with ProcessPoolExecutor(max_workers=workers, initializer=init, initargs=(bin_path,)) as pool:
        rows = list(pool.map(work, cases, chunksize=16))
    wall = time.perf_counter() - t0

    by_cat = {}
    for r in rows:
        by_cat.setdefault(r["category"], []).append(r)
    summary = {
        "n": len(rows),
        "recall": statistics.fmean(1.0 if r["ok"] else 0.0 for r in rows),
        "found_rate": statistics.fmean(1.0 if r["found"] else 0.0 for r in rows),
        "abstain_rate": statistics.fmean(1.0 if r["status"] == "abstain" else 0.0 for r in rows),
        "wall_s": round(wall, 3),
        "cases_per_s": round(len(rows) / wall, 1),
        "latency_us": {"mean": round(statistics.fmean(r["us"] for r in rows), 1),
                       "p50": round(sorted(r["us"] for r in rows)[len(rows)//2], 1)},
        "by_category": {k: {"n": len(v),
                            "recall": statistics.fmean(1.0 if x["ok"] else 0.0 for x in v),
                            "found_rate": statistics.fmean(1.0 if x["found"] else 0.0 for x in v)}
                        for k, v in sorted(by_cat.items())},
    }
    Path(out_path).parent.mkdir(parents=True, exist_ok=True)
    Path(out_path).write_text(json.dumps(summary, ensure_ascii=False, indent=2), encoding="utf-8")
    with Path(out_path).with_suffix(".cases.jsonl").open("w", encoding="utf-8") as f:
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    print(json.dumps({k: summary[k] for k in ("n", "recall", "found_rate", "abstain_rate", "wall_s", "latency_us")},
                     ensure_ascii=False))

if __name__ == "__main__":
    main()
