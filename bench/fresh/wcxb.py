#!/usr/bin/env python3
"""Web Content Extraction Benchmark (WCXB) evaluation driver.

Evaluates extractors across 2,008 pages in 7 categories (article, forum, product,
collection, listing, documentation, service).

Usage:
    python3 bench/fresh/wcxb.py prepare <wcxb-repo> <out-dir> [dev|test]
    python3 bench/fresh/wcxb.py score <wcxb-repo> <out-dir> [dev|test] <tool.jsonl>...
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


def prepare(repo: Path, out: Path, split: str = "dev") -> None:
    meta_path = repo / "metadata.json"
    if not meta_path.exists():
        sys.exit(f"Error: {meta_path} not found")
    metadata = json.loads(meta_path.read_text())

    html_in = repo / split / "html"
    html_out = out / "html"
    html_out.mkdir(parents=True, exist_ok=True)

    manifest_path = out / "manifest.jsonl"
    count = 0

    with manifest_path.open("w") as m:
        for file_id, info in metadata.get("files", {}).items():
            if info.get("split") != split:
                continue

            gz_path = html_in / f"{file_id}.html.gz"
            raw_path = html_in / f"{file_id}.html"

            dest_path = html_out / f"{file_id}.html"
            if gz_path.exists():
                dest_path.write_bytes(gzip.decompress(gz_path.read_bytes()))
            elif raw_path.exists():
                dest_path.write_bytes(raw_path.read_bytes())
            else:
                continue

            row = {
                "id": file_id,
                "path": str(dest_path.resolve()),
                "url": f"https://{info.get('domain', 'example.com')}",
                "page_type": info.get("page_type", "unknown"),
            }
            m.write(json.dumps(row) + "\n")
            count += 1

    print(f"Prepared {count} pages ({split}) in {out}", file=sys.stderr)


def score(repo: Path, out: Path, split: str, output_files: list[Path]) -> None:
    meta = json.loads((repo / "metadata.json").read_text())
    files_info = meta.get("files", {})
    gt_dir = repo / split / "ground-truth"

    for out_file in output_files:
        tool_name = out_file.stem
        predictions = {}
        latencies = []

        with out_file.open() as f:
            for line in f:
                if not line.strip():
                    continue
                row = json.loads(line)
                predictions[row["id"]] = plain(row.get("markdown", ""))
                if "ms" in row and row["ms"] is not None:
                    latencies.append(float(row["ms"]))

        by_type_f1 = defaultdict(list)
        all_f1 = []

        for file_id, pred_text in predictions.items():
            gt_json = gt_dir / f"{file_id}.json"
            gt_txt = gt_dir / f"{file_id}.txt"
            if gt_json.exists():
                try:
                    data = json.loads(gt_json.read_text(encoding="utf-8", errors="replace"))
                    ref_text = data.get("ground_truth", {}).get("main_content", "")
                except Exception:
                    continue
            elif gt_txt.exists():
                ref_text = gt_txt.read_text(encoding="utf-8", errors="replace")
            else:
                continue

            _, _, f1 = word_f1(pred_text, ref_text)

            page_type = files_info.get(file_id, {}).get("page_type", "unknown")
            by_type_f1[page_type].append(f1)
            all_f1.append(f1)

        median_ms = statistics.median(latencies) if latencies else 0.0
        mean_f1 = statistics.mean(all_f1) if all_f1 else 0.0

        print(f"\n=== WCXB [{split}] Tool: {tool_name} ===")
        print(f"Overall Mean F1: {mean_f1:.4f} (median latency: {median_ms:.2f} ms/page)")
        print(f"{'Page Type':<16} {'Count':<8} {'Mean F1':<10}")
        print("-" * 36)
        for ptype, scores in sorted(by_type_f1.items()):
            ptype_mean = statistics.mean(scores) if scores else 0.0
            print(f"{ptype:<16} {len(scores):<8} {ptype_mean:.4f}")


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        sys.exit(1)

    cmd = sys.argv[1]
    repo = Path(sys.argv[2])

    if cmd == "prepare":
        out = Path(sys.argv[3])
        split = sys.argv[4] if len(sys.argv) > 4 else "dev"
        prepare(repo, out, split)
    elif cmd == "score":
        out = Path(sys.argv[3])
        split = sys.argv[4] if len(sys.argv) > 4 else "dev"
        tool_outputs = [Path(p) for p in sys.argv[5:]]
        score(repo, out, split, tool_outputs)
    else:
        sys.exit(f"Unknown command: {cmd}")


if __name__ == "__main__":
    main()
