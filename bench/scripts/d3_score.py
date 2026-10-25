#!/usr/bin/env python3
"""D3 评分：复用官方 olmocr/bench/tests.py 的逐项判定。

性能要点：先把所有 markdown 预读进内存并建索引，再跑测试。
之前每项都重新 open 文件，7000 项 × 9 个参赛者 = 6 万次读盘，是慢的主因。
"""
import json, os, re, statistics, sys
from pathlib import Path

ROOT = Path("/home/yolow92/pagesift")
sys.path.insert(0, str(ROOT / "bench/upstream/olmocr"))
from olmocr.bench.tests import load_tests  # noqa: E402

BENCH = ROOT / "bench/data/olmocr_1403/bench_data"

def index_md(d: Path):
    """{(pdf_base, page): markdown}，文件内容预读。"""
    idx = {}
    for f in d.glob("**/*.md"):
        rel = f.relative_to(d).as_posix()
        m = re.match(r"^(.*)_pg(\d+)_repeat\d+\.md$", rel)
        if m:
            idx[(m.group(1), int(m.group(2)))] = f.read_text(encoding="utf-8", errors="replace")
    return idx

def index_tool(p: Path):
    """竞品输出：{pdf 相对路径: markdown}。"""
    rel = {}
    for line in (ROOT / "bench/data/showdown/d3/pdfs.jsonl").open(encoding="utf-8"):
        r = json.loads(line)
        rel[r["id"]] = r["rel"]
    out = {}
    for line in p.open(encoding="utf-8"):
        if not line.strip():
            continue
        r = json.loads(line)
        k = rel.get(r["id"])
        if k:
            out[k] = r.get("markdown") or ""
    return out

def score(getter, tests):
    per_type, per_group = {}, {}
    for group, t in tests:
        md = getter(t)
        if md is None:
            ok = False
        else:
            try:
                ok, _ = t.run(md)
            except Exception:
                ok = False
        v = 1.0 if ok else 0.0
        per_type.setdefault(t.type, []).append(v)
        per_group.setdefault(group, []).append(v)
    allv = [v for vals in per_group.values() for v in vals]
    return {"n": len(allv),
            "pass_rate": statistics.fmean(allv),
            "macro": statistics.fmean([statistics.fmean(v) for v in per_group.values()]),
            "groups": {k: {"n": len(v), "rate": statistics.fmean(v)}
                       for k, v in sorted(per_group.items())},
            "by_type": {k: {"n": len(v), "rate": statistics.fmean(v)}
                        for k, v in per_type.items()}}

def main():
    md070, md050, tools_dir, out = map(Path, sys.argv[1:5])
    tests = []
    for jp in sorted(BENCH.glob("*.jsonl")):
        tests.extend((jp.stem, t) for t in load_tests(str(jp)))
    print(f"tests={len(tests)} groups={len({g for g, _ in tests})}", file=sys.stderr)

    res = {}
    for tag, d in (("pagesift-070", md070), ("pagesift-050", md050)):
        idx = index_md(d)
        def getter(t, idx=idx):
            return idx.get((os.path.splitext(t.pdf)[0], t.page))
        res[tag] = score(getter, tests)
        print(f"  {tag} done", file=sys.stderr)

    for p in sorted(tools_dir.glob("*.jsonl")):
        by = index_tool(p)
        def getter(t, by=by):
            return by.get(t.pdf)
        res[p.stem] = score(getter, tests)
        print(f"  {p.stem} done", file=sys.stderr)

    out.write_text(json.dumps(res, ensure_ascii=False, indent=2), encoding="utf-8")
    rows = sorted(res.items(), key=lambda kv: -kv[1]["macro"])
    print(f"{'参赛者':<18}{'宏平均':>9}{'微平均':>9}{'项数':>8}")
    for k, v in rows:
        print(f"{k:<18}{v['macro']:>9.4f}{v['pass_rate']:>9.4f}{v['n']:>8}")

if __name__ == "__main__":
    main()
