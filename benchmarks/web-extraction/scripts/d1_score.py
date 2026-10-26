#!/usr/bin/env python3
"""D1 评分：复用 bench/fresh/wcxb.py 的 plain()/word_f1()（即 0.50 报告用的同一指标），
配对改用本目录 manifest 的内嵌 ground-truth，避免 ID 命名差异。

输出每个参赛者的整体 F1/P/R 与按页面类型的 F1。
"""
import json, statistics, sys
from pathlib import Path

sys.path.insert(0, "/home/yolow92/pagesift/bench/fresh")
from wcxb import plain, word_f1  # noqa: E402

D1 = Path("/home/yolow92/pagesift/bench/data/showdown/d1")
OUT = Path("/home/yolow92/pagesift/out/d1")
REPORT = Path("/home/yolow92/pagesift/out/d1/score.json")

man = {}
for line in (D1 / "manifest.jsonl").open(encoding="utf-8"):
    if line.strip():
        r = json.loads(line)
        man[r["id"]] = r

def score_file(p: Path):
    preds = {}
    for line in p.open(encoding="utf-8"):
        if not line.strip():
            continue
        try:
            r = json.loads(line)
        except Exception:
            continue
        preds[r["id"]] = r.get("markdown") or ""
    f1s, ps, rs = [], [], []
    by_type = {}
    missing = 0
    for pid, m in man.items():
        pred = preds.get(pid)
        if pred is None:
            missing += 1
            pred = ""
        pr, rc, f1 = word_f1(plain(pred), plain(m["gt"]))
        f1s.append(f1); ps.append(pr); rs.append(rc)
        by_type.setdefault(m["page_type"], []).append(f1)
    return {
        "n": len(f1s), "missing": missing,
        "f1": statistics.fmean(f1s) if f1s else 0.0,
        "precision": statistics.fmean(ps) if ps else 0.0,
        "recall": statistics.fmean(rs) if rs else 0.0,
        "by_type": {k: {"n": len(v), "f1": statistics.fmean(v)} for k, v in sorted(by_type.items())},
    }

res = {}
for f in sorted(OUT.glob("*.jsonl")):
    tag = f.stem
    if tag in ("score",):
        continue
    try:
        res[tag] = score_file(f)
    except Exception as e:
        print(f"{tag}: 评分失败 {e}", file=sys.stderr)

meta = {}
for f in sorted(OUT.glob("*.meta.json")):
    d = json.load(open(f))
    meta[d["contestant"]] = d

for tag, r in res.items():
    if tag in meta:
        r["wall_s"] = meta[tag]["wall_s"]
        r["peak_rss_bytes"] = meta[tag]["peak_rss_bytes"]
        r["oom"] = meta[tag]["oom"]
        r["pages_per_s"] = r["n"] / r["wall_s"] if r["wall_s"] else 0.0

REPORT.write_text(json.dumps(res, ensure_ascii=False, indent=2), encoding="utf-8")
rows = sorted(res.items(), key=lambda kv: -kv[1]["f1"])
print(f"{'参赛者':<22}{'F1':>8}{'P':>8}{'R':>8}{'页/秒':>9}{'峰值MiB':>9}")
for tag, r in rows:
    print(f"{tag:<22}{r['f1']:>8.4f}{r['precision']:>8.4f}{r['recall']:>8.4f}"
          f"{r.get('pages_per_s',0):>9.1f}{r.get('peak_rss_bytes',0)/1048576:>9.1f}")
