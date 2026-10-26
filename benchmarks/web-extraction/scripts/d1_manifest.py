#!/usr/bin/env python3
"""D1 正文提取：解压 WCXB 2,008 页并生成统一 manifest。

WCXB 原始 HTML 是 .html.gz；这里解到 showdown/d1/html/ 下，manifest 里给容器内路径。
字段: {id, path, url, page_type, gt}
"""
import gzip, json, collections
from pathlib import Path

ROOT = Path("/home/yolow92/pagesift/benchmarks/data/fresh/data/wcxb")
OUT = Path("/home/yolow92/pagesift/benchmarks/data/showdown/d1")
HTML = OUT / "html"
HTML.mkdir(parents=True, exist_ok=True)

rows, missing_gt, dup = [], 0, 0
for split in ("dev", "test"):
    for src in sorted((ROOT / split / "html").glob("*.html.gz")):
        fid = src.name.replace(".html.gz", "")
        dst = HTML / f"{split}-{fid}.html"
        if not dst.exists():
            dst.write_bytes(gzip.decompress(src.read_bytes()))
        gt_path = ROOT / split / "ground-truth" / f"{fid}.json"
        if not gt_path.exists():
            missing_gt += 1
            continue
        g = json.loads(gt_path.read_text(encoding="utf-8"))
        gt = g.get("ground_truth") or {}
        text = gt.get("content") or gt.get("text") or gt.get("markdown") or ""
        if not text:
            text = " ".join(v for v in gt.values() if isinstance(v, str))
        if not text.strip():
            missing_gt += 1
            continue
        pt = ((g.get("_internal") or {}).get("page_type") or {}).get("primary", "unknown")
        rows.append({"id": f"{split}-{fid}",
                     "path": f"/data/showdown/d1/html/{dst.name}",
                     "url": g.get("url", ""), "page_type": pt, "gt": text})

with (OUT / "manifest.jsonl").open("w", encoding="utf-8") as f:
    for r in rows:
        f.write(json.dumps(r, ensure_ascii=False) + "\n")
print(f"pages={len(rows)} missing_gt={missing_gt}")
print("page_type:", dict(collections.Counter(r["page_type"] for r in rows)))
print("avg_gt_chars:", sum(len(r["gt"]) for r in rows)//max(len(rows),1))
