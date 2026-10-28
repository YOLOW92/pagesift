"""第 2 版 A3 评分（RULES.md 第二节）。只用标准库。

    python3 -I benchmarks/mix/v2/score_a3.py <参照目录 1> <参照目录 2> <运行目录> <工具>... > v2/results/quality-a3.md

- 参照词：两次 Firefox 渲染都出现的词（小写，\\w+ 切分），排除轮播、时间这类会变的内容。两次都没拿到的页不评覆盖率。
- 完成：工具这一份状态 ok 且输出至少 200 字。
- 覆盖率：参照词里出现在工具输出中的比例，逐页平均；没完成的页按 0 算（全部页），另报只算所有工具都完成的页。
- 耗时、峰值内存来自 driver 的 results.jsonl。
"""
import json
import re
import statistics
import sys
from pathlib import Path

WORD = re.compile(r"\w+", re.UNICODE)


def words(text):
    return set(w.lower() for w in WORD.findall(text))


def main():
    ref1, ref2, runs = Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3])
    tools = sys.argv[4:]
    res = {}
    for t in tools:
        rows = {}
        for line in (runs / t / "results.jsonl").read_text().splitlines():
            r = json.loads(line)
            rows[r["id"]] = r
        res[t] = rows
    ids = sorted(set().union(*(set(r) for r in res.values())))
    ref = {}
    for i in ids:
        a, b = ref1 / f"{i}.txt", ref2 / f"{i}.txt"
        if a.exists() and b.exists():
            w = words(a.read_text(encoding="utf-8", errors="replace")) & words(b.read_text(encoding="utf-8", errors="replace"))
            if w:
                ref[i] = w

    def out_text(t, i):
        p = runs / t / f"{i}.md"
        return p.read_text(encoding="utf-8", errors="replace") if p.exists() else ""

    done = {t: {i for i in ids if res[t].get(i, {}).get("status") == "ok" and len(out_text(t, i)) >= 200} for t in tools}
    common = set(ref).intersection(*done.values()) if done else set()
    print("# A3 需要 JS 渲染的网页（实时抓）\n")
    print(f"共 {len(ids)} 页；两次参照渲染都拿到文字的 {len(ref)} 页；所有工具都完成的 {len(common)} 页。\n")
    print("| 工具 | 完成率 ↑ | 参照词覆盖率 ↑（全部，没完成按 0） | 参照词覆盖率 ↑（都完成的页） | 单页中位耗时 ms ↓ | p95 ms ↓ | 单页峰值内存最大 MB ↓ |")
    print("|---|---|---|---|---|---|---|")
    summary = {}
    for t in tools:
        cov_all, cov_common = [], []
        for i in ref:
            c = len(ref[i] & words(out_text(t, i))) / len(ref[i]) if i in done[t] else 0.0
            cov_all.append(c)
            if i in common:
                cov_common.append(c)
        ms = sorted(r["ms"] for r in res[t].values() if r.get("status") == "ok" and r.get("ms") is not None)
        peak = max((r.get("peak_mb") or 0 for r in res[t].values()), default=0)
        s = {"completion": len(done[t]) / len(ids), "coverage_all": statistics.mean(cov_all) if cov_all else None,
             "coverage_common": statistics.mean(cov_common) if cov_common else None,
             "p50": statistics.median(ms) if ms else None, "p95": ms[min(len(ms) - 1, int(0.95 * len(ms)))] if ms else None, "peak": peak}
        summary[t] = s
        f = lambda x, d=3: "—" if x is None else f"{x:.{d}f}"  # noqa: E731
        print(f"| {t} | {f(s['completion'])} | {f(s['coverage_all'])} | {f(s['coverage_common'])} | {f(s['p50'], 0)} | {f(s['p95'], 0)} | {f(s['peak'], 0)} |")
    (runs / "a3-summary.json").write_text(json.dumps({"n": len(ids), "n_ref": len(ref), "n_common": len(common), "tools": summary}, indent=1))


if __name__ == "__main__":
    main()
