#!/usr/bin/env python3
"""WCEB 抽取驱动：对 8 套数据集逐页调用 pagesift runner，产出 Markdown。

用法: wceb_run.py <combined_dir> <out_dir> <runner_bin> [threads]
"""
import json, subprocess, sys, tempfile
from pathlib import Path

DATASETS = ["cetd", "cleaneval", "cleanportaleval", "dragnet",
            "google-trends-2017", "l3s-gn1", "readability", "scrapinghub"]

def main():
    combined = Path(sys.argv[1]); out = Path(sys.argv[2])
    runner = sys.argv[3]; threads = sys.argv[4] if len(sys.argv) > 4 else "64"
    out.mkdir(parents=True, exist_ok=True)
    for ds in DATASETS:
        gt_path = combined / "ground-truth" / f"{ds}.jsonl"
        html_dir = combined / "html" / ds
        if not gt_path.exists():
            print(f"[skip] {ds}: no ground truth"); continue
        rows = [json.loads(l) for l in gt_path.open(encoding="utf-8")]
        manifest = out / f"{ds}.in.jsonl"
        n = 0
        with manifest.open("w", encoding="utf-8") as f:
            for i, r in enumerate(rows):
                hp = html_dir / f"{r['page_id']}.html"
                if not hp.exists():
                    continue
                html = hp.read_text(encoding="utf-8", errors="replace")
                f.write(json.dumps({"track_id": r["page_id"], "html": html,
                                    "url": "", "row": i}, ensure_ascii=False) + "\n")
                n += 1
        parts = out / f"{ds}.parts.jsonl"
        subprocess.run([runner, str(manifest), str(parts), threads,
                        "--hard-timeout-secs", "180", "--soft-warn-secs", "60"],
                       stderr=subprocess.DEVNULL, check=False)
        print(f"[done] {ds}: html={n} gt={len(rows)}", flush=True)

if __name__ == "__main__":
    main()
