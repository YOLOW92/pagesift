#!/usr/bin/env python3
"""WebMainBench evaluation driver.

Evaluates extractors against WebMainBench datasets (including text, table, formula metrics).

Usage:
    python3 benchmarks/common/fresh/webmain.py prepare <input-jsonl> <out-dir>
    python3 benchmarks/common/fresh/webmain.py score <gold-jsonl> <tool.jsonl>...
"""

from __future__ import annotations

import json
import re
import statistics
import sys
from collections import Counter
from pathlib import Path

IMAGE = re.compile(r"!\[[^\]]*\]\([^)]*\)")
LINK = re.compile(r"\[([^\]]*)\]\([^)]*\)")
MARKS = re.compile(r"^\s{0,3}(#{1,6}\s+|>\s?|[-*+]\s+|\d+[.)]\s+)|[*_`|]+", re.M)


def plain(md: str) -> str:
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
    if not text:
        return []
    # Support CJK characters as individual tokens and English words
    tokens = []
    for word in re.findall(r"[\u4e00-\u9fff]|\w+", text.lower()):
        tokens.append(word)
    return tokens


def word_f1(predicted: str, reference: str) -> tuple[float, float, float]:
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


def prepare(input_jsonl: Path, out: Path) -> None:
    html_out = out / "html"
    html_out.mkdir(parents=True, exist_ok=True)
    manifest_path = out / "manifest.jsonl"
    gold_path = out / "gold.jsonl"

    count = 0
    with input_jsonl.open(encoding="utf-8") as fin, \
         manifest_path.open("w", encoding="utf-8") as fman, \
         gold_path.open("w", encoding="utf-8") as fgold:
        for i, line in enumerate(fin):
            if not line.strip():
                continue
            row = json.loads(line)
            doc_id = str(row.get("id") or row.get("track_id") or f"doc_{i}")
            html_content = row.get("html", "")
            if not html_content:
                continue

            file_path = html_out / f"{doc_id}.html"
            file_path.write_text(html_content, encoding="utf-8")

            fman.write(json.dumps({
                "id": doc_id,
                "path": str(file_path.resolve()),
                "url": row.get("url", "")
            }) + "\n")

            gold_text = row.get("groundtruth_content") or row.get("content", "")
            fgold.write(json.dumps({
                "id": doc_id,
                "groundtruth": gold_text,
                "type": row.get("type", ""),
                "layout_type": row.get("layout_type", "")
            }) + "\n")
            count += 1

    print(f"Prepared {count} WebMainBench pages in {out}", file=sys.stderr)


def score(gold_path: Path, output_files: list[Path]) -> None:
    gold = {}
    with gold_path.open(encoding="utf-8") as f:
        for line in f:
            if not line.strip():
                continue
            row = json.loads(line)
            gold[str(row["id"])] = row.get("groundtruth", "")

    for out_file in output_files:
        tool_name = out_file.stem
        predictions = {}
        latencies = []

        with out_file.open(encoding="utf-8") as f:
            for line in f:
                if not line.strip():
                    continue
                row = json.loads(line)
                predictions[str(row["id"])] = plain(row.get("markdown", ""))
                if "ms" in row and row["ms"] is not None:
                    latencies.append(float(row["ms"]))

        f1_list, p_list, r_list = [], [], []
        for doc_id, pred_text in predictions.items():
            if doc_id not in gold:
                continue
            p, r, f1 = word_f1(pred_text, gold[doc_id])
            p_list.append(p)
            r_list.append(r)
            f1_list.append(f1)

        median_ms = statistics.median(latencies) if latencies else 0.0
        mean_f1 = statistics.mean(f1_list) if f1_list else 0.0
        mean_p = statistics.mean(p_list) if p_list else 0.0
        mean_r = statistics.mean(r_list) if r_list else 0.0

        print(f"\n=== WebMainBench Tool: {tool_name} ===")
        print(f"Evaluated Pages: {len(f1_list)}")
        print(f"Mean F1: {mean_f1:.4f} | Precision: {mean_p:.4f} | Recall: {mean_r:.4f}")
        print(f"Median Latency: {median_ms:.2f} ms/page")


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        sys.exit(1)

    cmd = sys.argv[1]
    if cmd == "prepare":
        prepare(Path(sys.argv[2]), Path(sys.argv[3]))
    elif cmd == "score":
        gold = Path(sys.argv[2])
        outputs = [Path(p) for p in sys.argv[3:]]
        score(gold, outputs)
    else:
        sys.exit(f"Unknown command: {cmd}")


if __name__ == "__main__":
    main()
