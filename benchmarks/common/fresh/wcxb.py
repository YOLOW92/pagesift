#!/usr/bin/env python3
"""Web Content Extraction Benchmark (WCXB) evaluation driver.

Evaluates extractors across all 2,008 pages in 7 categories (article, forum, product,
collection, listing, documentation, service).

Usage:
    python3 benchmarks/common/fresh/wcxb.py prepare <wcxb-repo> <out-dir> [dev|test|all]
    python3 benchmarks/common/fresh/wcxb.py score <wcxb-repo> <out-dir> [dev|test|all] <tool.jsonl>...
"""

from __future__ import annotations

import argparse
import gzip
import json
import re
import statistics
import sys
from collections import Counter, defaultdict
from pathlib import Path

IMAGE = re.compile(r"!\[[^\]]*\]\([^)]*\)")
LINK = re.compile(r"\[([^\]]*)\]\([^)]*\)")
MARKS = re.compile(r"^\s{0,3}(#{1,6}\s+|>\s?|[-*+]\s+|\d+[.)]\s+)|[*_`|]+", re.M)


def plain(md: str) -> str:
    """Markdown → text: images removed, links reduced to their text, formatting stripped."""
    text = IMAGE.sub("", md)
    prev = None
    while prev != text:
        prev, text = text, LINK.sub(lambda m: m.group(1), text)
    text = re.sub(r"\\([!-/:-@\[-`{-~])", r"\1", text)
    text = MARKS.sub("", text)
    return "\n".join(
        line.strip()
        for line in text.splitlines()
        if line.strip() and set(line.strip()) - set("-:= ")
    )


def tokenize(text: str) -> list[str]:
    """Tokenize text into lowercase words."""
    if not text:
        return []
    return re.findall(r"\w+", text.lower())


def word_f1(predicted: str, reference: str) -> tuple[float, float, float]:
    """Compute word-level precision, recall, and F1."""
    pred_tokens = tokenize(predicted)
    ref_tokens = tokenize(reference)

    if not ref_tokens:
        return (1.0, 1.0, 1.0) if not pred_tokens else (0.0, 0.0, 0.0)
    if not pred_tokens:
        return (0.0, 0.0, 0.0)

    pred_counts = Counter(pred_tokens)
    ref_counts = Counter(ref_tokens)

    overlap = sum((pred_counts & ref_counts).values())
    p = overlap / len(pred_tokens)
    r = overlap / len(ref_tokens)
    f1 = 2 * p * r / (p + r) if (p + r) > 0 else 0.0
    return p, r, f1


def get_gt_info(gt_path: Path) -> tuple[str, str, str]:
    """Return (main_content, url, page_type) from ground truth file."""
    if not gt_path.exists():
        return "", "", "unknown"
    try:
        data = json.loads(gt_path.read_text(encoding="utf-8", errors="replace"))
        ref_text = data.get("ground_truth", {}).get("main_content", "") or ""
        url = data.get("url", "")
        internal = data.get("_internal", {}) or {}
        pt_obj = internal.get("page_type", {})
        if isinstance(pt_obj, dict):
            pt = pt_obj.get("primary", "article")
        elif isinstance(pt_obj, str):
            pt = pt_obj
        else:
            pt = "article"
        if pt == "category":
            pt = "collection"
        return ref_text, url, pt
    except Exception:
        return "", "", "unknown"


def prepare(repo: Path, out: Path, split: str = "all") -> None:
    splits = ["dev", "test"] if split == "all" else [split]
    html_out = out / "html"
    html_out.mkdir(parents=True, exist_ok=True)
    manifest_path = out / "manifest.jsonl"
    count = 0

    with manifest_path.open("w") as m:
        for s in splits:
            html_in = repo / s / "html"
            gt_dir = repo / s / "ground-truth"
            if not html_in.exists():
                continue

            for html_file in sorted(html_in.iterdir()):
                file_id = html_file.name.replace(".html.gz", "").replace(".html", "")
                if not file_id:
                    continue

                dest_path = html_out / f"{file_id}.html"
                if not dest_path.exists():
                    if html_file.name.endswith(".gz"):
                        dest_path.write_bytes(gzip.decompress(html_file.read_bytes()))
                    else:
                        dest_path.write_bytes(html_file.read_bytes())

                gt_path = gt_dir / f"{file_id}.json"
                _, url, page_type = get_gt_info(gt_path)
                if not url:
                    url = f"https://example.com/{file_id}"

                row = {
                    "id": file_id,
                    "path": str(dest_path.resolve()),
                    "url": url,
                    "page_type": page_type,
                    "split": s,
                }
                m.write(json.dumps(row) + "\n")
                count += 1

    print(f"Prepared {count} pages ({split}) in {out}", file=sys.stderr)


def score(repo: Path, out: Path, split: str, output_files: list[Path]) -> None:
    splits = ["dev", "test"] if split == "all" else [split]
    gt_dirs = [repo / s / "ground-truth" for s in splits]

    # Pre-index all GT info
    all_gt = {}
    for gtd in gt_dirs:
        if not gtd.exists():
            continue
        for f in gtd.glob("*.json"):
            ref_text, url, page_type = get_gt_info(f)
            all_gt[f.stem] = {
                "ref_text": ref_text,
                "url": url,
                "page_type": page_type,
            }

    for out_file in output_files:
        tool_name = out_file.stem
        predictions = {}
        latencies = []

        with out_file.open() as f:
            for line in f:
                if not line.strip():
                    continue
                try:
                    row = json.loads(line)
                    predictions[row["id"]] = plain(row.get("markdown", ""))
                    if "ms" in row and row["ms"] is not None:
                        latencies.append(float(row["ms"]))
                except Exception:
                    continue

        by_type_p = defaultdict(list)
        by_type_r = defaultdict(list)
        by_type_f1 = defaultdict(list)
        all_p = []
        all_r = []
        all_f1 = []

        for file_id, pred_text in predictions.items():
            if file_id not in all_gt:
                continue
            gt = all_gt[file_id]
            ref_text = gt["ref_text"]
            page_type = gt["page_type"]

            p, r, f1 = word_f1(pred_text, ref_text)

            by_type_p[page_type].append(p)
            by_type_r[page_type].append(r)
            by_type_f1[page_type].append(f1)
            all_p.append(p)
            all_r.append(r)
            all_f1.append(f1)

        median_ms = statistics.median(latencies) if latencies else 0.0
        mean_ms = statistics.mean(latencies) if latencies else 0.0
        p95_ms = sorted(latencies)[int(len(latencies) * 0.95)] if latencies else 0.0
        p99_ms = sorted(latencies)[int(len(latencies) * 0.99)] if latencies else 0.0
        mean_p = statistics.mean(all_p) if all_p else 0.0
        mean_r = statistics.mean(all_r) if all_r else 0.0
        mean_f1 = statistics.mean(all_f1) if all_f1 else 0.0

        print(f"\n==================================================")
        print(f"=== WCXB [{split.upper()}] Evaluation: {tool_name} (Evaluated: {len(all_f1)}/{len(all_gt)}) ===")
        print(f"==================================================")
        print(f"Overall Metrics:")
        print(f"  Precision: {mean_p:.4f}  |  Recall: {mean_r:.4f}  |  F1: {mean_f1:.4f}")
        print(f"Latency Percentiles:")
        print(f"  Mean: {mean_ms:.2f} ms  |  p50 (Median): {median_ms:.2f} ms  |  p95: {p95_ms:.2f} ms  |  p99: {p99_ms:.2f} ms")
        print(f"-" * 50)
        print(f"{'Page Type':<16} {'Count':<8} {'Prec':<8} {'Rec':<8} {'F1':<8}")
        print(f"-" * 50)
        for ptype, scores in sorted(by_type_f1.items()):
            p_m = statistics.mean(by_type_p[ptype]) if by_type_p[ptype] else 0.0
            r_m = statistics.mean(by_type_r[ptype]) if by_type_r[ptype] else 0.0
            f1_m = statistics.mean(scores) if scores else 0.0
            print(f"{ptype:<16} {len(scores):<8} {p_m:<8.4f} {r_m:<8.4f} {f1_m:<8.4f}")
        print(f"==================================================\n")


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        sys.exit(1)

    cmd = sys.argv[1]
    repo = Path(sys.argv[2])

    if cmd == "prepare":
        out = Path(sys.argv[3])
        split = sys.argv[4] if len(sys.argv) > 4 else "all"
        prepare(repo, out, split)
    elif cmd == "score":
        out = Path(sys.argv[3])
        split = sys.argv[4] if len(sys.argv) > 4 else "all"
        tool_outputs = [Path(p) for p in sys.argv[5:]]
        score(repo, out, split, tool_outputs)
    else:
        sys.exit(f"Unknown command: {cmd}")


if __name__ == "__main__":
    main()
